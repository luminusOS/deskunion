; DeskUnion — Inno Setup installer script
;
; Preprocessor defines passed by CI:
;   AppVersion    — e.g. "0.1.0"
;   SourceDir     — path to the bundled Windows folder
;   OutputDir     — where to write the installer exe
;   SetupIconFile — optional .ico path

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\windows\deskunion"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

[Setup]
AppName=DeskUnion
AppVersion={#AppVersion}
AppVerName=DeskUnion
AppId={{B9B448DF-64D3-4E7A-BD99-1D9B5E29C518}
VersionInfoVersion={#AppVersion}
AppPublisher=LuminusOS
AppPublisherURL=https://github.com/luminusOS/deskunion
AppSupportURL=https://github.com/luminusOS/deskunion/issues
AppUpdatesURL=https://github.com/luminusOS/deskunion/releases
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\DeskUnion
DefaultGroupName=DeskUnion
UninstallDisplayIcon={app}\bin\deskunion.exe
OutputDir={#OutputDir}
OutputBaseFilename=deskunion-setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
DisableProgramGroupPage=auto
CloseApplications=yes
CloseApplicationsFilter=deskunion.exe
SetupLogging=yes
#ifdef SetupIconFile
SetupIconFile={#SetupIconFile}
#endif
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\DeskUnion"; Filename: "{app}\bin\deskunion.exe"
Name: "{group}\Uninstall DeskUnion"; Filename: "{uninstallexe}"
Name: "{autodesktop}\DeskUnion"; Filename: "{app}\bin\deskunion.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\bin\deskunion.exe"; Description: "Launch DeskUnion"; Flags: nowait postinstall skipifsilent
