; Velarouter NSIS installer (requires makensis)
!define APPNAME "Velarouter"
!ifndef VERSION
!define VERSION "0.1.0"
!endif
Name "${APPNAME} ${VERSION}"
OutFile "..\..\dist\Velarouter-${VERSION}-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\${APPNAME}"
RequestExecutionLevel user
Icon "..\..\assets\brand\app-icons\windows\velarouter.ico"

Page directory
Page instfiles

Section "Install"
  SetOutPath "$INSTDIR"
  File "..\..\dist\Velarouter-windows-x64\Velarouter.exe"
  File "..\..\dist\Velarouter-windows-x64\Velarouter.ico"
  File "..\..\dist\Velarouter-windows-x64\LICENSE"
  CreateShortcut "$DESKTOP\${APPNAME}.lnk" "$INSTDIR\Velarouter.exe" "" "$INSTDIR\Velarouter.ico"
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortcut "$SMPROGRAMS\${APPNAME}\${APPNAME}.lnk" "$INSTDIR\Velarouter.exe" "" "$INSTDIR\Velarouter.ico"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\Velarouter.exe"
  Delete "$INSTDIR\Velarouter.ico"
  Delete "$INSTDIR\Uninstall.exe"
  Delete "$INSTDIR\LICENSE"
  Delete "$DESKTOP\${APPNAME}.lnk"
  RMDir /r "$SMPROGRAMS\${APPNAME}"
  RMDir "$INSTDIR"
SectionEnd
