; Kill running app processes before file replacement so NSIS can overwrite
; locked binaries and bundled web/ resources (old UI stays if files are locked).
!macro NSIS_HOOK_PREINSTALL
    nsis_tauri_utils::KillProcessCurrentUser "mexc-trading-bot-desktop.exe"
    Pop $R0
    nsis_tauri_utils::KillProcessCurrentUser "mexc-trading-bot.exe"
    Pop $R0
    ; Give WebView2 / file locks time to release after process kill.
    Sleep 3000
!macroend

!macro NSIS_HOOK_PREUNINSTALL
    nsis_tauri_utils::KillProcessCurrentUser "mexc-trading-bot-desktop.exe"
    Pop $R0
    nsis_tauri_utils::KillProcessCurrentUser "mexc-trading-bot.exe"
    Pop $R0
    Sleep 1500
!macroend
