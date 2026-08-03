use std::{
    env, fs,
    mem::size_of,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use windows::{
    Win32::{
        Foundation::{CloseHandle, RPC_E_CHANGED_MODE, VARIANT_FALSE, VARIANT_TRUE},
        NetworkManagement::WindowsFirewall::{
            INetFwPolicy2, INetFwRule, INetFwRules, NET_FW_ACTION_ALLOW, NET_FW_IP_PROTOCOL,
            NET_FW_IP_PROTOCOL_TCP, NET_FW_IP_PROTOCOL_UDP, NET_FW_PROFILE2_ALL,
            NET_FW_RULE_DIR_IN, NetFwPolicy2, NetFwRule,
        },
        Storage::FileSystem::{GetFullPathNameW, WIN32_FIND_DATAW},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, IPersistFile, STGM_READ,
            },
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                TH32CS_SNAPPROCESS,
            },
            Environment::ExpandEnvironmentStringsW,
            Threading::{
                GetExitCodeProcess, INFINITE, OpenProcess, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, WaitForSingleObject,
            },
        },
        UI::{
            Shell::{
                IShellLinkW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, SLGP_RAWPATH,
                ShellExecuteExW, ShellLink,
            },
            WindowsAndMessaging::SW_HIDE,
        },
    },
    core::{BSTR, Interface, PCWSTR},
};
use windows_registry::CURRENT_USER;

const HELPER_SWITCH: &str = "--nearweave-installer-helper";
const UNINSTALL_ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const PUBLISHER: &str = "railgun20001";
const CURRENT_PRODUCT_NAME: &str = "NearWeave";
const CURRENT_BINARY_NAME: &str = "nearweave.exe";
const UDP_RULE_NAME: &str = "NearWeave LAN Discovery (UDP 37991)";
const TCP_RULE_NAME: &str = "NearWeave LAN Transfer (Dynamic TCP)";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MigrationState {
    found: bool,
    restore_autostart: bool,
}

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

/// 返回 `Some` 表示当前进程以安装辅助模式启动，调用方必须直接使用该退出码结束进程。
/// 辅助模式只接受固定动作，不提供任意命令、路径或防火墙规则入口。
pub fn run_from_args() -> Option<i32> {
    let mut arguments = env::args_os().skip(1);
    if arguments.next()?.to_string_lossy() != HELPER_SWITCH {
        return None;
    }

    let action = arguments
        .next()
        .map(|value| value.to_string_lossy().into_owned());
    let result = match action.as_deref() {
        Some("migrate-pre") => required_path_argument(&mut arguments)
            .and_then(|state_file| run_preinstall_migration(&state_file)),
        Some("migrate-post") => required_path_argument(&mut arguments)
            .and_then(|state_file| run_postinstall_migration(&state_file)),
        Some("firewall-add") if arguments.next().is_none() => configure_firewall(true),
        Some("firewall-remove") if arguments.next().is_none() => configure_firewall(false),
        Some("firewall-present") if arguments.next().is_none() => firewall_rules_are_present()
            .then_some(())
            .ok_or_else(|| "未配置 NearWeave 局域网防火墙规则".into()),
        _ => Err("安装辅助参数无效".into()),
    };

    Some(if result.is_ok() { 0 } else { 1 })
}

fn required_path_argument(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<PathBuf, String> {
    let path = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| "缺少安装迁移状态文件路径".to_string())?;
    if arguments.next().is_some() {
        return Err("安装辅助参数过多".into());
    }
    Ok(path)
}

fn run_preinstall_migration(state_file: &Path) -> Result<(), String> {
    let old_product_name = legacy_string(&[34013, 26725]);
    let old_binary_name = legacy_string(&[
        98, 108, 117, 101, 116, 111, 111, 116, 104, 45, 115, 104, 97, 114, 101, 46, 101, 120, 101,
    ]);
    let local_app_data = required_environment_path("LOCALAPPDATA")?;
    let expected_old_install = normalize_path(&local_app_data.join(&old_product_name))?;
    let expected_old_program = normalize_path(&expected_old_install.join(&old_binary_name))?;
    let expected_old_uninstaller = normalize_path(&expected_old_install.join("uninstall.exe"))?;

    let uninstall_root = CURRENT_USER.open(UNINSTALL_ROOT).ok();
    let mut candidates = Vec::new();
    if let Some(root) = uninstall_root.as_ref() {
        for subkey_name in root.keys().map_err(registry_error)? {
            let Ok(entry) = root.open(&subkey_name) else {
                continue;
            };
            if entry.get_string("DisplayName").ok().as_deref() == Some(old_product_name.as_str()) {
                candidates.push(subkey_name);
            }
        }
    }

    if candidates.len() > 1 {
        return Err("检测到多个旧版当前用户安装项，已中止安装".into());
    }
    if candidates.is_empty() {
        if expected_old_program.is_file() {
            return Err("检测到没有有效卸载项的旧版程序，已中止安装".into());
        }
        write_migration_state(
            state_file,
            &MigrationState {
                found: false,
                restore_autostart: false,
            },
        )?;
        return Ok(());
    }

    let subkey_name = &candidates[0];
    let root = uninstall_root.ok_or_else(|| "旧版卸载根注册表项不存在".to_string())?;
    let entry = root.open(subkey_name).map_err(registry_error)?;
    if entry.get_string("Publisher").map_err(registry_error)? != PUBLISHER {
        return Err("旧版安装项发布者不匹配，已中止安装".into());
    }
    let version = entry.get_string("DisplayVersion").map_err(registry_error)?;
    if !legacy_version_is_supported(&version) {
        return Err("旧版安装项版本不在允许迁移的范围内，已中止安装".into());
    }
    let install_location = entry
        .get_string("InstallLocation")
        .map_err(registry_error)?;
    if !paths_equal(Path::new(&install_location), &expected_old_install)? {
        return Err("旧版安装路径异常，已中止安装".into());
    }
    let uninstall_command = entry
        .get_string("UninstallString")
        .map_err(registry_error)?;
    let uninstall_path = command_path(&uninstall_command)
        .ok_or_else(|| "旧版卸载程序命令无效，已中止安装".to_string())?;
    if !paths_equal(&uninstall_path, &expected_old_uninstaller)? {
        return Err("旧版卸载程序路径异常，已中止安装".into());
    }
    if !expected_old_uninstaller.is_file() || !expected_old_program.is_file() {
        return Err("旧版安装文件不完整，已中止安装".into());
    }
    if process_is_running_at(&old_binary_name, &expected_old_program)? {
        return Err("旧版程序仍在运行，请退出后重试".into());
    }

    let restore_autostart = CURRENT_USER
        .open(RUN_KEY)
        .ok()
        .and_then(|key| key.get_string(&old_product_name).ok())
        .and_then(|command| command_path(&command))
        .is_some_and(|path| paths_equal(&path, &expected_old_program).unwrap_or(false));
    write_migration_state(
        state_file,
        &MigrationState {
            found: true,
            restore_autostart,
        },
    )?;

    let status = Command::new(&expected_old_uninstaller)
        .arg("/S")
        .status()
        .map_err(|error| format!("无法启动旧版卸载程序：{error}"))?;
    if !status.success() {
        return Err(format!(
            "旧版卸载程序返回错误码 {}，已中止安装",
            status.code().unwrap_or(-1)
        ));
    }

    for _ in 0..20 {
        let registry_exists = root.open(subkey_name).is_ok();
        if !expected_old_program.exists() && !registry_exists {
            break;
        }
        thread::sleep(Duration::from_millis(250));
    }
    if expected_old_program.exists() || root.open(subkey_name).is_ok() {
        return Err("旧版卸载未完整结束，已中止安装".into());
    }

    let shortcut_name = format!("{old_product_name}.lnk");
    remove_verified_shortcut(
        &required_environment_path("USERPROFILE")?
            .join("Desktop")
            .join(&shortcut_name),
        &expected_old_program,
    )?;
    remove_verified_shortcut(
        &required_environment_path("APPDATA")?
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join(shortcut_name),
        &expected_old_program,
    )?;
    Ok(())
}

fn run_postinstall_migration(state_file: &Path) -> Result<(), String> {
    let result = (|| {
        let state: MigrationState = serde_json::from_slice(
            &fs::read(state_file).map_err(|error| format!("安装迁移状态文件不存在：{error}"))?,
        )
        .map_err(|error| format!("安装迁移状态文件无效：{error}"))?;
        if !state.restore_autostart {
            return Ok(());
        }

        let current_program = normalize_path(
            &env::current_exe().map_err(|error| format!("无法读取 NearWeave 程序路径：{error}"))?,
        )?;
        let expected_program = expected_installed_program()?;
        if !paths_equal(&current_program, &expected_program)? || !current_program.is_file() {
            return Err("NearWeave 安装路径异常，无法恢复开机启动".into());
        }
        CURRENT_USER
            .create(RUN_KEY)
            .and_then(|key| {
                key.set_string(
                    CURRENT_PRODUCT_NAME,
                    format!(r#""{}""#, current_program.display()),
                )
            })
            .map_err(registry_error)
    })();
    let _ = fs::remove_file(state_file);
    result
}

fn write_migration_state(path: &Path, state: &MigrationState) -> Result<(), String> {
    let bytes =
        serde_json::to_vec(state).map_err(|error| format!("无法编码安装迁移状态：{error}"))?;
    fs::write(path, bytes).map_err(|error| format!("无法写入安装迁移状态：{error}"))
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

    for name in current_and_legacy_rule_names() {
        let _ = unsafe { rules.Remove(&BSTR::from(name)) };
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
    current_and_legacy_rule_names()
        .iter()
        .any(|name| unsafe { rules.Item(&BSTR::from(name)) }.is_ok())
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

fn current_and_legacy_rule_names() -> Vec<String> {
    vec![
        UDP_RULE_NAME.into(),
        TCP_RULE_NAME.into(),
        legacy_string(&[
            34013, 26725, 32, 23616, 22495, 32593, 21457, 29616, 32, 40, 85, 68, 80, 32, 51, 55,
            57, 57, 49, 41,
        ]),
        legacy_string(&[
            34013, 26725, 32, 23616, 22495, 32593, 20256, 36755, 32, 40, 21160, 24577, 32, 84, 67,
            80, 41,
        ]),
        legacy_string(&[
            66, 108, 117, 101, 98, 114, 105, 100, 103, 101, 32, 76, 65, 78, 32, 68, 105, 115, 99,
            111, 118, 101, 114, 121, 32, 40, 85, 68, 80, 32, 51, 55, 57, 57, 49, 41,
        ]),
        legacy_string(&[
            66, 108, 117, 101, 98, 114, 105, 100, 103, 101, 32, 76, 65, 78, 32, 84, 114, 97, 110,
            115, 102, 101, 114, 32, 40, 68, 121, 110, 97, 109, 105, 99, 32, 84, 67, 80, 41,
        ]),
    ]
}

fn process_is_running_at(binary_name: &str, expected_path: &Path) -> Result<bool, String> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|error| format!("无法枚举旧版进程：{error}"))?;
    let result = (|| {
        let mut entry = PROCESSENTRY32W {
            dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX),
            ..Default::default()
        };
        if unsafe { Process32FirstW(snapshot, &mut entry) }.is_err() {
            return Ok(false);
        }
        loop {
            let name = string_from_wide_nul(&entry.szExeFile);
            if name.eq_ignore_ascii_case(binary_name)
                && process_path(entry.th32ProcessID)
                    .is_some_and(|path| paths_equal(&path, expected_path).unwrap_or(false))
            {
                return Ok(true);
            }
            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
        Ok(false)
    })();
    let _ = unsafe { CloseHandle(snapshot) };
    result
}

fn process_path(process_id: u32) -> Option<PathBuf> {
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()?;
    let mut buffer = vec![0_u16; 32_768];
    let mut length = u32::try_from(buffer.len()).ok()?;
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };
    let _ = unsafe { CloseHandle(process) };
    result
        .ok()
        .map(|_| PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize])))
}

fn remove_verified_shortcut(shortcut_path: &Path, expected_target: &Path) -> Result<(), String> {
    if !shortcut_path.is_file() {
        return Ok(());
    }
    let _com = ComApartment::initialize()?;
    let shortcut: IShellLinkW = unsafe {
        CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|error| format!("无法读取旧版快捷方式：{error}"))?
    };
    let persisted: IPersistFile = shortcut
        .cast()
        .map_err(|error| format!("无法读取旧版快捷方式：{error}"))?;
    let shortcut_wide = wide_null(shortcut_path.as_os_str());
    unsafe { persisted.Load(PCWSTR(shortcut_wide.as_ptr()), STGM_READ) }
        .map_err(|error| format!("无法加载旧版快捷方式：{error}"))?;
    let mut target = vec![0_u16; 32_768];
    let mut metadata = WIN32_FIND_DATAW::default();
    unsafe { shortcut.GetPath(&mut target, &mut metadata, SLGP_RAWPATH.0 as u32) }
        .map_err(|error| format!("无法解析旧版快捷方式：{error}"))?;
    let target = PathBuf::from(string_from_wide_nul(&target));
    if paths_equal(&target, expected_target)? {
        fs::remove_file(shortcut_path).map_err(|error| format!("无法删除旧版快捷方式：{error}"))?;
    }
    Ok(())
}

fn legacy_version_is_supported(value: &str) -> bool {
    parse_version(value).is_some_and(|version| ((0, 1, 0)..=(0, 3, 1)).contains(&version))
}

fn parse_version(value: &str) -> Option<(u32, u32, u32)> {
    let mut parts = value.split('.');
    let parsed = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(parsed)
}

fn command_path(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    if let Some(remainder) = command.strip_prefix('"') {
        let end = remainder.find('"')?;
        return Some(PathBuf::from(&remainder[..end]));
    }
    command.split_whitespace().next().map(PathBuf::from)
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

fn legacy_string(value: &[u16]) -> String {
    String::from_utf16(value).expect("旧版兼容名称必须是有效 UTF-16")
}

fn registry_error(error: impl std::fmt::Display) -> String {
    format!("Windows 注册表操作失败：{error}")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_supported_legacy_versions_are_accepted() {
        assert!(legacy_version_is_supported("0.1.0"));
        assert!(legacy_version_is_supported("0.3.1"));
        assert!(!legacy_version_is_supported("0.0.9"));
        assert!(!legacy_version_is_supported("0.3.2"));
        assert!(!legacy_version_is_supported("0.3.1.1"));
        assert!(!legacy_version_is_supported("invalid"));
    }

    #[test]
    fn command_path_handles_quoted_and_plain_uninstall_commands() {
        assert_eq!(
            command_path(r#""C:\Program Files\NearWeave\uninstall.exe" /S"#),
            Some(PathBuf::from(r"C:\Program Files\NearWeave\uninstall.exe"))
        );
        assert_eq!(
            command_path(r"C:\NearWeave\uninstall.exe /S"),
            Some(PathBuf::from(r"C:\NearWeave\uninstall.exe"))
        );
    }

    #[test]
    fn legacy_names_are_reconstructed_without_loss() {
        assert_eq!(legacy_string(&[34013, 26725]).chars().count(), 2);
        assert_eq!(current_and_legacy_rule_names().len(), 6);
    }
}
