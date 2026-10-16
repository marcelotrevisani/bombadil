; Inno Setup script for the Windows installer. The release workflow runs
;   iscc /DAppVersion=<version> packaging\windows\bombadil.iss
; after `cargo build --release`, and the setup lands in dist\.
;
; Per-user by default (no admin prompt), with the choice to install for all
; users. The shortcuts point at bombadil.ico because the exe carries no icon
; resource of its own.

#ifndef AppVersion
  #error Pass the version: iscc /DAppVersion=x.y.z bombadil.iss
#endif

[Setup]
AppId={{6F0B7C55-2B1E-4C8E-9F0B-5A7B2C1D9E41}
AppName=Bombadil
AppVersion={#AppVersion}
AppPublisher=Marcelo Duarte Trevisani
AppPublisherURL=https://github.com/marcelotrevisani/bombadil
AppSupportURL=https://github.com/marcelotrevisani/bombadil/issues
DefaultDirName={autopf}\Bombadil
DefaultGroupName=Bombadil
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\..\LICENSE
SetupIconFile=bombadil.ico
UninstallDisplayIcon={app}\bombadil.ico
OutputDir=..\..\dist
OutputBaseFilename=bombadil-{#AppVersion}-x86_64-windows-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\..\target\release\bombadil.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "bombadil.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Bombadil"; Filename: "{app}\bombadil.exe"; IconFilename: "{app}\bombadil.ico"
Name: "{autodesktop}\Bombadil"; Filename: "{app}\bombadil.exe"; IconFilename: "{app}\bombadil.ico"; Tasks: desktopicon

[Run]
Filename: "{app}\bombadil.exe"; Description: "{cm:LaunchProgram,Bombadil}"; Flags: nowait postinstall skipifsilent
