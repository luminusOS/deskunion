#define AppName "DeskUnion"
#define AppVersion GetEnv("DESKUNION_VERSION")
#define AppPublisher "LuminusOS"
#define AppExeName "deskunion.exe"

[Setup]
AppId={{B9B448DF-64D3-4E7A-BD99-1D9B5E29C518}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={autopf}\DeskUnion
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
OutputDir=dist
OutputBaseFilename=DeskUnion-{#AppVersion}-Windows-x86_64-Setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayIcon={app}\bin\{#AppExeName}
SetupIconFile=crates\deskunion-gtk\resources\deskunion.ico
WizardStyle=modern

[Files]
Source: "deskunion-windows\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\bin\{#AppExeName}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\bin\{#AppExeName}"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Run]
Filename: "{app}\bin\{#AppExeName}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent
