; Inno Setup script for the Locus installer: locus-<version>-windows-x64-setup.exe.
; Built by release.yml:  ISCC /DAppVersion=<version> /DSourceExe=<path to locus.exe> installer\locus.iss
; Per-user install by default (no admin); /ALLUSERS installs for every user. User data in
; %LOCALAPPDATA%\Locus is left alone on uninstall.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\locus.exe"
#endif

[Setup]
; Never change AppId: winget and upgrades find the installed app by it.
AppId={{EF342699-9418-4381-B7E9-7D6307F08DA3}
AppName=Locus
AppVersion={#AppVersion}
AppVerName=Locus {#AppVersion}
AppPublisher=sao-coding
AppPublisherURL=https://github.com/sao-coding/locus
AppSupportURL=https://github.com/sao-coding/locus/issues
AppUpdatesURL=https://github.com/sao-coding/locus/releases
DefaultDirName={autopf}\Locus
DefaultGroupName=Locus
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir=..\dist
OutputBaseFilename=locus-{#AppVersion}-windows-x64-setup
SetupIconFile=..\crates\app\assets\app.ico
UninstallDisplayIcon={app}\locus.exe
UninstallDisplayName=Locus
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "locus.exe"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\THIRD_PARTY_LICENSES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Locus"; Filename: "{app}\locus.exe"
Name: "{autodesktop}\Locus"; Filename: "{app}\locus.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\locus.exe"; Description: "{cm:LaunchProgram,Locus}"; Flags: nowait postinstall skipifsilent
