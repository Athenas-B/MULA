; NSIS installer hooks (bundle.windows.nsis.installerHooks in tauri.conf.json).

; MULA keeps its data in %APPDATA%\mula (util::mula_config_dir), not in the bundle-id folder the
; uninstaller's "Delete the application data" checkbox clears, so clear that folder too: settings,
; logs, Wall Changer caches and VSD's Python environment (tens of MB).
!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    SetShellVarContext current
    RmDir /r "$APPDATA\mula"
  ${EndIf}
!macroend
