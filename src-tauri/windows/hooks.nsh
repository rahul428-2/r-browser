; Tauri NSIS installer hook (bundle.windows.nsis.installerHooks).
; Tauri's template leaves the installer's CompanyName empty, which antivirus heuristics count against it.
VIAddVersionKey "CompanyName" "Rahul Varma"
