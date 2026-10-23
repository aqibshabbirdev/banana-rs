; Banana Photos for Windows: installs for the current user (no admin rights needed), with a
; Start menu entry, an optional desktop icon and an uninstaller. Built by the Windows workflow:
;   iscc /DVersion=0.1.4 /DSource=dist\win scripts\windows-installer.iss

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Source
  #define Source "..\dist\win"
#endif

[Setup]
AppId={{6B1C1E0D-6C2E-4E59-9A55-BA7A7A7A0001}
AppName=Banana Photos
AppVersion={#Version}
AppPublisher=Aqib Shabbir
AppPublisherURL=https://github.com/aqibshabbirdev/banana-rs
DefaultDirName={localappdata}\Programs\Banana Photos
DefaultGroupName=Banana Photos
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=BananaPhotos-{#Version}-x64-setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\banana-rs.exe
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#Source}\banana-rs.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\models\*"; DestDir: "{app}\models"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\Banana Photos"; Filename: "{app}\banana-rs.exe"
Name: "{autodesktop}\Banana Photos"; Filename: "{app}\banana-rs.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\banana-rs.exe"; Description: "{cm:LaunchProgram,Banana Photos}"; Flags: nowait postinstall skipifsilent
