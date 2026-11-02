; Setup needs elevation once; the installed GUI and viewer do not.
!macro NSIS_HOOK_PREINSTALL
  nsExec::ExecToStack '"$SYSDIR\sc.exe" stop NexusAgent'
  Pop $0
  Pop $1
  Sleep 1500
!macroend

!macro NSIS_HOOK_POSTINSTALL
  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "$INSTDIR\install-desktop.ps1" -BundleDirectory "$INSTDIR" -InstallDirectory "$INSTDIR" -InPlace -NoShortcuts'
  Pop $0
  Pop $1
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Link Tunnel host setup failed: $1"
    Abort
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; Keep identity, pairings and all configuration for a later reinstall.
  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File "$INSTDIR\uninstall-desktop.ps1" -InstallDirectory "$INSTDIR"'
  Pop $0
  Pop $1
!macroend
