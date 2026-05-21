Set WshShell = CreateObject("WScript.Shell")
Set FSO = CreateObject("Scripting.FileSystemObject")

scriptDir = FSO.GetParentFolderName(WScript.ScriptFullName)
launcher = FSO.BuildPath(scriptDir, "openrelay.exe")
If Not FSO.FileExists(launcher) Then
  launcher = FSO.BuildPath(scriptDir, "target\release\openrelay.exe")
End If

If FSO.FileExists(launcher) Then
  WshShell.CurrentDirectory = scriptDir
  WshShell.Environment("PROCESS")("OPENRELAY_ROOT") = scriptDir
  WshShell.Run """" & launcher & """", 0, False
Else
  WshShell.Popup "未找到 Rust release 可执行文件：" & vbCrLf & launcher & vbCrLf & vbCrLf & "请先运行 start.bat 编译并启动。", 10, "OpenRelay", 48
End If

Set WshShell = Nothing
Set FSO = Nothing
