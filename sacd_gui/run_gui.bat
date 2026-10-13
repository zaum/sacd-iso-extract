@echo off
rem Start the SACD -> DSF Extractor GUI without a console window.
rem Works from any location: paths are relative to this file.
start "" pythonw.exe "%~dp0app.py"
if errorlevel 1 (
    echo Could not start Python. Make sure pythonw.exe is on PATH.
    pause
)
