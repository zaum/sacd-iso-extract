' Launch the SACD -> DSF Extractor GUI without flashing a console window.
' Double-click this file to start the app silently (pythonw, no terminal).
Option Explicit
Dim shell, fso, appDir
Set shell = CreateObject("WScript.Shell")
Set fso = CreateObject("Scripting.FileSystemObject")
appDir = fso.GetParentFolderName(WScript.ScriptFullName)
shell.CurrentDirectory = appDir

On Error Resume Next
' pythonw.exe runs GUI apps with no console window at all.
shell.Run "pythonw.exe """ & appDir & "\app.py""", 0, False
If Err.Number <> 0 Then
    Err.Clear
    ' Fallback: plain python.exe, window hidden (style 0).
    shell.Run "python.exe """ & appDir & "\app.py""", 0, False
    If Err.Number <> 0 Then
        MsgBox "Could not start Python. Make sure Python is installed and on PATH.", _
               vbCritical, "SACD Extractor"
    End If
End If
