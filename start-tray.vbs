' Compatibility shim for existing shortcuts.
Set WshShell = CreateObject("WScript.Shell")
Set FSO = CreateObject("Scripting.FileSystemObject")

scriptDir = FSO.GetParentFolderName(WScript.ScriptFullName)
launcher = scriptDir & "\start-tray.exe"

If FSO.FileExists(launcher) Then
  WshShell.Run """" & launcher & """", 0, False
Else
  WshShell.Popup "start-tray.exe not found." & vbCrLf & "Build it from start-tray.c first.", 7, "OpenRelay", 48
End If

Set WshShell = Nothing
Set FSO = Nothing
