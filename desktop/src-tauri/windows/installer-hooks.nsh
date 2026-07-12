; Kill the running desktop process before file replacement so upgrades
; overwrite the main binary and bundled resources (NSIS skips locked files).
!macro NSIS_HOOK_PREINSTALL
    nsis_tauri_utils::KillProcessCurrentUser "mexc-trading-bot-desktop.exe"
    Pop $R0
    Sleep 1500
!macroend

!macro NSIS_HOOK_PREUNINSTALL
    nsis_tauri_utils::KillProcessCurrentUser "mexc-trading-bot-desktop.exe"
    Pop $R0
    Sleep 1000
!macroend
