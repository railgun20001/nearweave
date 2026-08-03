use std::{
    env,
    mem::size_of,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, RPC_E_CHANGED_MODE, VARIANT_FALSE, VARIANT_TRUE},
        NetworkManagement::WindowsFirewall::{
            INetFwPolicy2, INetFwRule, INetFwRules, NET_FW_ACTION_ALLOW, NET_FW_IP_PROTOCOL,
            NET_FW_IP_PROTOCOL_TCP, NET_FW_IP_PROTOCOL_UDP, NET_FW_PROFILE2_ALL,
            NET_FW_RULE_DIR_IN, NetFwPolicy2, NetFwRule,
        },
        Storage::FileSystem::GetFullPathNameW,
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            Environment::ExpandEnvironmentStringsW,
            Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
        },
        UI::{
            Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW},
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::{BSTR, PCWSTR},
};

const HELPER_SWITCH: &str = "--nearweave-installer-helper";
const CURRENT_PRODUCT_NAME: &str = "NearWeave";
const CURRENT_BINARY_NAME: &str = "nearweave.exe";
const UDP_RULE_NAME: &str = "NearWeave LAN Discovery (UDP 37991)";
const TCP_RULE_NAME: &str = "NearWeave LAN Transfer (Dynamic TCP)";

#[derive(Clone, Copy)]
struct FirewallRuleSpec {
    name: &'static str,
    description: &'static str,
    protocol: NET_FW_IP_PROTOCOL,
    local_ports: &'static str,
}

const FIREWALL_RULES: [FirewallRuleSpec; 2] = [
    FirewallRuleSpec {
        name: UDP_RULE_NAME,
        description: "Allow NearWeave discovery from the local subnet and Windows hotspot",
        protocol: NET_FW_IP_PROTOCOL_UDP,
        local_ports: "37991",
    },
    FirewallRuleSpec {
        name: TCP_RULE_NAME,
        description: "Allow NearWeave dynamic TCP transfer from the local subnet and Windows hotspot",
        protocol: NET_FW_IP_PROTOCOL_TCP,
        local_ports: "*",
    },
];

/// 返回 `Some` 表示当前进程以特权辅助模式启动，调用方必须直接使用该退出码结束进程。
/// 辅助模式只接受固定防火墙动作，不提供任意命令、路径或规则入口。
pub fn run_from_args() -> Option<i32> {
    let mut arguments = env::args_os().skip(1);
    if arguments.next()?.to_string_lossy() != HELPER_SWITCH {
        return None;
    }

    let action = arguments
        .next()
        .map(|value| value.to_string_lossy().into_owned());
    let result = match action.as_deref() {
        Some("firewall-add") if arguments.next().is_none() => configure_firewall(true),
        Some("firewall-remove") if arguments.next().is_none() => configure_firewall(false),
        Some("firewall-present") if arguments.next().is_none() => firewall_rules_are_present()
            .then_some(())
            .ok_or_else(|| "未配置 NearWeave 局域网防火墙规则".into()),
        _ => Err("特权辅助参数无效".into()),
    };

    Some(if result.is_ok() { 0 } else { 1 })
}

fn configure_firewall(add: bool) -> Result<(), String> {
    let program = expected_installed_program()?;
    if add {
        let current = normalize_path(
            &env::current_exe().map_err(|error| format!("无法读取 NearWeave 程序路径：{error}"))?,
        )?;
        if !paths_equal(&current, &program)? || !current.is_file() {
            return Err("只能为已安装的 NearWeave 主程序配置防火墙".into());
        }
    }

    let _com = ComApartment::initialize()?;
    let policy: INetFwPolicy2 = unsafe {
        CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER)
            .map_err(|error| format!("无法访问 Windows 防火墙策略：{error}"))?
    };
    let rules = unsafe { policy.Rules() }
        .map_err(|error| format!("无法访问 Windows 防火墙规则：{error}"))?;

    for spec in FIREWALL_RULES {
        let _ = unsafe { rules.Remove(&BSTR::from(spec.name)) };
    }
    if !add {
        return Ok(());
    }

    for spec in FIREWALL_RULES {
        let rule = create_firewall_rule(spec, &program)?;
        if let Err(error) = unsafe { rules.Add(&rule) } {
            for rollback in FIREWALL_RULES {
                let _ = unsafe { rules.Remove(&BSTR::from(rollback.name)) };
            }
            return Err(format!("无法添加 Windows 防火墙规则：{error}"));
        }
    }
    Ok(())
}

/// 只读校验当前安装路径的两条防火墙规则，启动时调用不会触发 UAC。
pub(crate) fn lan_firewall_is_configured() -> bool {
    expected_installed_program()
        .and_then(|program| firewall_rules_are_current(&program))
        .unwrap_or(false)
}

fn firewall_rules_are_present() -> bool {
    let Ok(_com) = ComApartment::initialize() else {
        return false;
    };
    let Ok(policy): Result<INetFwPolicy2, _> =
        (unsafe { CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER) })
    else {
        return false;
    };
    let Ok(rules) = (unsafe { policy.Rules() }) else {
        return false;
    };
    FIREWALL_RULES
        .iter()
        .any(|spec| unsafe { rules.Item(&BSTR::from(spec.name)) }.is_ok())
}

/// 用户明确选择启用局域网时调用。规则缺失才通过 `runas` 启动同一份已签名 GUI
/// 程序；辅助进程没有控制台，系统只显示 Windows UAC。
pub(crate) fn ensure_lan_firewall_access() -> Result<(), String> {
    if lan_firewall_is_configured() {
        return Ok(());
    }

    let program = expected_installed_program()?;
    let current = normalize_path(
        &env::current_exe().map_err(|error| format!("无法读取 NearWeave 程序路径：{error}"))?,
    )?;
    if !paths_equal(&current, &program)? || !current.is_file() {
        return Err("请先安装 NearWeave，再启用局域网传输".into());
    }

    let verb = wide_null(std::ffi::OsStr::new("runas"));
    let program_wide = wide_null(program.as_os_str());
    let parameters = wide_null(std::ffi::OsStr::new(
        "--nearweave-installer-helper firewall-add",
    ));
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: u32::try_from(size_of::<SHELLEXECUTEINFOW>()).unwrap_or(u32::MAX),
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(program_wide.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut execute) }
        .map_err(|_| "Windows 管理员授权已取消或无法启动".to_string())?;
    if execute.hProcess.is_invalid() {
        return Err("无法等待 Windows 防火墙授权进程".into());
    }

    let wait_result = unsafe { WaitForSingleObject(execute.hProcess, INFINITE) };
    let mut exit_code = u32::MAX;
    let exit_result = unsafe { GetExitCodeProcess(execute.hProcess, &mut exit_code) };
    let _ = unsafe { CloseHandle(execute.hProcess) };
    if wait_result != windows::Win32::Foundation::WAIT_OBJECT_0 || exit_result.is_err() {
        return Err("等待 Windows 防火墙授权结果失败".into());
    }
    if exit_code != 0 {
        return Err("未能写入 NearWeave 局域网防火墙规则".into());
    }
    if !lan_firewall_is_configured() {
        return Err("Windows 未保留 NearWeave 局域网防火墙规则".into());
    }
    Ok(())
}

fn firewall_rules_are_current(program: &Path) -> Result<bool, String> {
    let _com = ComApartment::initialize()?;
    let policy: INetFwPolicy2 = unsafe {
        CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER)
            .map_err(|error| format!("无法访问 Windows 防火墙策略：{error}"))?
    };
    let rules = unsafe { policy.Rules() }
        .map_err(|error| format!("无法访问 Windows 防火墙规则：{error}"))?;
    Ok(FIREWALL_RULES
        .iter()
        .all(|spec| firewall_rule_is_current(&rules, *spec, program)))
}

fn firewall_rule_is_current(rules: &INetFwRules, spec: FirewallRuleSpec, program: &Path) -> bool {
    let Ok(rule) = (unsafe { rules.Item(&BSTR::from(spec.name)) }) else {
        return false;
    };
    let Ok(application_name) = (unsafe { rule.ApplicationName() }) else {
        return false;
    };
    paths_equal(Path::new(&application_name.to_string()), program).unwrap_or(false)
        && unsafe { rule.Protocol() }.is_ok_and(|value| value == spec.protocol.0)
        && unsafe { rule.LocalPorts() }
            .is_ok_and(|value| value.to_string().eq_ignore_ascii_case(spec.local_ports))
        && unsafe { rule.RemoteAddresses() }
            .is_ok_and(|value| value.to_string().eq_ignore_ascii_case("LocalSubnet"))
        && unsafe { rule.Direction() }.is_ok_and(|value| value == NET_FW_RULE_DIR_IN)
        && unsafe { rule.Profiles() }.is_ok_and(|value| value == NET_FW_PROFILE2_ALL.0)
        && unsafe { rule.EdgeTraversal() }.is_ok_and(|value| value == VARIANT_FALSE)
        && unsafe { rule.Action() }.is_ok_and(|value| value == NET_FW_ACTION_ALLOW)
        && unsafe { rule.Enabled() }.is_ok_and(|value| value == VARIANT_TRUE)
}

fn create_firewall_rule(spec: FirewallRuleSpec, program: &Path) -> Result<INetFwRule, String> {
    let rule: INetFwRule = unsafe {
        CoCreateInstance(&NetFwRule, None, CLSCTX_INPROC_SERVER)
            .map_err(|error| format!("无法创建 Windows 防火墙规则：{error}"))?
    };
    let configure = || unsafe {
        rule.SetName(&BSTR::from(spec.name))?;
        rule.SetDescription(&BSTR::from(spec.description))?;
        rule.SetApplicationName(&BSTR::from(program.to_string_lossy().as_ref()))?;
        rule.SetProtocol(spec.protocol.0)?;
        rule.SetLocalPorts(&BSTR::from(spec.local_ports))?;
        rule.SetRemoteAddresses(&BSTR::from("LocalSubnet"))?;
        rule.SetDirection(NET_FW_RULE_DIR_IN)?;
        rule.SetProfiles(NET_FW_PROFILE2_ALL.0)?;
        rule.SetEdgeTraversal(VARIANT_FALSE)?;
        rule.SetAction(NET_FW_ACTION_ALLOW)?;
        rule.SetEnabled(VARIANT_TRUE)
    };
    configure().map_err(|error| format!("无法设置 Windows 防火墙规则：{error}"))?;
    Ok(rule)
}

fn required_environment_path(name: &str) -> Result<PathBuf, String> {
    env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("缺少 Windows 环境变量 {name}"))
}

fn expected_installed_program() -> Result<PathBuf, String> {
    normalize_path(
        &required_environment_path("LOCALAPPDATA")?
            .join(CURRENT_PRODUCT_NAME)
            .join(CURRENT_BINARY_NAME),
    )
}

fn paths_equal(left: &Path, right: &Path) -> Result<bool, String> {
    Ok(normalize_path(left)?
        .to_string_lossy()
        .eq_ignore_ascii_case(&normalize_path(right)?.to_string_lossy()))
}

fn normalize_path(path: &Path) -> Result<PathBuf, String> {
    let source = wide_null(path.as_os_str());
    let expanded_length = unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), None) };
    if expanded_length == 0 {
        return Err("无法展开安装路径中的环境变量".into());
    }
    let mut expanded = vec![0_u16; expanded_length as usize];
    if unsafe { ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), Some(&mut expanded)) } == 0 {
        return Err("无法展开安装路径中的环境变量".into());
    }
    let expanded = string_from_wide_nul(&expanded);
    let expanded_wide = wide_null(std::ffi::OsStr::new(&expanded));
    let full_length = unsafe { GetFullPathNameW(PCWSTR(expanded_wide.as_ptr()), None, None) };
    if full_length == 0 {
        return Err("无法规范化安装路径".into());
    }
    let mut full = vec![0_u16; full_length as usize + 1];
    if unsafe { GetFullPathNameW(PCWSTR(expanded_wide.as_ptr()), Some(&mut full), None) } == 0 {
        return Err("无法规范化安装路径".into());
    }
    let normalized = string_from_wide_nul(&full)
        .trim_end_matches(['\\', '/'])
        .to_string();
    Ok(PathBuf::from(normalized))
}

fn wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn string_from_wide_nul(value: &[u16]) -> String {
    let length = value
        .iter()
        .position(|code| *code == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..length])
}

struct ComApartment {
    should_uninitialize: bool,
}

impl ComApartment {
    fn initialize() -> Result<Self, String> {
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result.is_ok() {
            Ok(Self {
                should_uninitialize: true,
            })
        } else if result == RPC_E_CHANGED_MODE {
            Ok(Self {
                should_uninitialize: false,
            })
        } else {
            Err(format!("无法初始化 Windows COM：{result:?}"))
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.should_uninitialize {
            unsafe { CoUninitialize() };
        }
    }
}
