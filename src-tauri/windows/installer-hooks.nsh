VIAddVersionKey "CompanyName" "railgun20001"

!macro NEARWEAVE_RUN_PREINSTALL_MIGRATION
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=nearweave-installer-helper.exe "${MAINBINARYSRCPATH}"
  DetailPrint "正在检查旧版安装并迁移安装状态..."
  ExecWait '"$PLUGINSDIR\nearweave-installer-helper.exe" --nearweave-installer-helper migrate-pre "$PLUGINSDIR\nearweave-migration.json"' $0
  IntCmp $0 0 nearweave_preinstall_ok
  MessageBox MB_ICONSTOP|MB_OK "无法安全迁移旧版安装。请退出旧程序并检查安装信息后重试。"
  Abort
nearweave_preinstall_ok:
  SetOutPath "$INSTDIR"
!macroend

!macro NEARWEAVE_RUN_POSTINSTALL_MIGRATION
  DetailPrint "正在恢复 NearWeave 安装状态..."
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --nearweave-installer-helper migrate-post "$PLUGINSDIR\nearweave-migration.json"' $0
  IntCmp $0 0 nearweave_postinstall_ok
  MessageBox MB_ICONEXCLAMATION|MB_OK "NearWeave 已安装，但旧版开机启动状态未能恢复。"
nearweave_postinstall_ok:
!macroend

!macro NEARWEAVE_RUN_FIREWALL ACTION
  DetailPrint "正在检查 NearWeave 局域网防火墙规则..."
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --nearweave-installer-helper firewall-present' $0
  IntCmp $0 0 nearweave_firewall_cleanup nearweave_firewall_done nearweave_firewall_done
nearweave_firewall_cleanup:
  ExecShellWait "runas" "$INSTDIR\${MAINBINARYNAME}.exe" '--nearweave-installer-helper firewall-${ACTION}' SW_HIDE
nearweave_firewall_done:
  SetOutPath "$INSTDIR"
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro NEARWEAVE_RUN_PREINSTALL_MIGRATION
!macroend

!macro NSIS_HOOK_POSTINSTALL
  !insertmacro NEARWEAVE_RUN_POSTINSTALL_MIGRATION
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro NEARWEAVE_RUN_FIREWALL "remove"
!macroend
