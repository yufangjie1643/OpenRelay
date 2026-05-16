' LiteLLM Proxy + Web UI - No console window, system tray icon
Set WshShell = CreateObject("WScript.Shell")
Set FSO = CreateObject("Scripting.FileSystemObject")
scriptDir = FSO.GetParentFolderName(WScript.ScriptFullName)

' Start LiteLLM Proxy (hidden window)
WshShell.Run """" & scriptDir & "\.venv\Scripts\litellm.exe"" --config """ & scriptDir & "\litellm-config.yaml"" --port 4000", 0, False

' Start Web UI with tray icon (hidden window)
WshShell.Run """" & scriptDir & "\.venv\Scripts\node.exe"" """ & scriptDir & "\web\server.js""", 0, False

' Show startup notification
WshShell.Popup "LiteLLM Proxy started" & vbCrLf & "Web UI: http://localhost:8080" & vbCrLf & "Proxy: http://localhost:4000", 2, "LiteLLM Proxy", 64

Set WshShell = Nothing
Set FSO = Nothing
