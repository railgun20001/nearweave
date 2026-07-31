!define NEARWEAVE_HOOK_DIR "${__FILEDIR__}"
VIAddVersionKey "CompanyName" "railgun20001"

!macro NEARWEAVE_RUN_PREINSTALL_MIGRATION
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=nearweave-migrate-install.ps1 "${NEARWEAVE_HOOK_DIR}\migrate-legacy-install.ps1"
  DetailPrint "正在检查旧版安装并迁移安装状态..."
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "$PLUGINSDIR\nearweave-migrate-install.ps1" -Phase PreInstall -StateFile "$PLUGINSDIR\nearweave-migration.json"' $0
  IntCmp $0 0 nearweave_preinstall_ok
  MessageBox MB_ICONSTOP|MB_OK "无法安全迁移旧版安装。请退出旧程序并检查安装信息后重试。"
  Abort
nearweave_preinstall_ok:
  SetOutPath "$INSTDIR"
!macroend

!macro NEARWEAVE_RUN_POSTINSTALL_MIGRATION
  DetailPrint "正在恢复 NearWeave 安装状态..."
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "$PLUGINSDIR\nearweave-migrate-install.ps1" -Phase PostInstall -StateFile "$PLUGINSDIR\nearweave-migration.json" -Program "$INSTDIR\${MAINBINARYNAME}.exe"' $0
  IntCmp $0 0 nearweave_postinstall_ok
  MessageBox MB_ICONEXCLAMATION|MB_OK "NearWeave 已安装，但旧版开机启动状态未能恢复。"
nearweave_postinstall_ok:
!macroend

!macro NEARWEAVE_RUN_FIREWALL ACTION
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=nearweave-configure-firewall.ps1 "${NEARWEAVE_HOOK_DIR}\configure-firewall.ps1"
  DetailPrint "正在更新 NearWeave 局域网防火墙规则..."
  ExecShellWait "runas" "$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" '-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "$PLUGINSDIR\nearweave-configure-firewall.ps1" -Action ${ACTION} -Program "$INSTDIR\${MAINBINARYNAME}.exe"' SW_HIDE
  SetOutPath "$INSTDIR"
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro NEARWEAVE_RUN_PREINSTALL_MIGRATION
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro NEARWEAVE_RUN_FIREWALL "Add"
  !insertmacro NEARWEAVE_RUN_POSTINSTALL_MIGRATION
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro NEARWEAVE_RUN_FIREWALL "Remove"
!macroend
