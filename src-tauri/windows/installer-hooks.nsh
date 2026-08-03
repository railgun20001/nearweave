VIAddVersionKey "CompanyName" "railgun20001"

!macro NEARWEAVE_RUN_FIREWALL ACTION
  DetailPrint "正在检查 NearWeave 局域网防火墙规则..."
  ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --nearweave-installer-helper firewall-present' $0
  IntCmp $0 0 nearweave_firewall_cleanup nearweave_firewall_done nearweave_firewall_done
nearweave_firewall_cleanup:
  ExecShellWait "runas" "$INSTDIR\${MAINBINARYNAME}.exe" '--nearweave-installer-helper firewall-${ACTION}' SW_HIDE
nearweave_firewall_done:
  SetOutPath "$INSTDIR"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  StrCmp $UpdateMode 1 nearweave_firewall_uninstall_done
  !insertmacro NEARWEAVE_RUN_FIREWALL "remove"
nearweave_firewall_uninstall_done:
!macroend
