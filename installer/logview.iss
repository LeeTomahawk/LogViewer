; Instalator logview (Inno Setup 6).
; Budowany przez scripts/package.ps1, który przekazuje AppVersion i SourceExe:
;   ISCC /DAppVersion=0.1.0 /DSourceExe=..\target\dist\logview.exe installer\logview.iss

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\dist\logview.exe"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif

[Setup]
AppId={{7C1D4E2A-5B3F-4A9E-9C61-2F8B0D3E6A41}
AppName=logview
AppVersion={#AppVersion}
AppVerName=logview {#AppVersion}
AppPublisher=logview
AppComments=Przeglądarka dużych plików logów
DefaultDirName={autopf}\logview
DefaultGroupName=logview
DisableProgramGroupPage=yes
OutputDir={#OutputDir}
OutputBaseFilename=logview-{#AppVersion}-setup
SetupIconFile=..\assets\logview.ico
UninstallDisplayIcon={app}\logview.exe
UninstallDisplayName=logview
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
PrivilegesRequired=admin
ChangesAssociations=yes
CloseApplications=yes

[Languages]
Name: "polish"; MessagesFile: "compiler:Languages\Polish.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
polish.ContextMenu=Dodaj „Otwórz w logview” do menu kontekstowego plików logów (.log, .txt, .out, .jsonl, .gz, .zst)
english.ContextMenu=Add "Open in logview" to the context menu of log files (.log, .txt, .out, .jsonl, .gz, .zst)
polish.Explorer=Integracja z Eksploratorem:
english.Explorer=Explorer integration:
polish.OpenIn=Otwórz w logview
english.OpenIn=Open in logview

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "contextmenu"; Description: "{cm:ContextMenu}"; GroupDescription: "{cm:Explorer}"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\logview"; Filename: "{app}\logview.exe"
Name: "{autodesktop}\logview"; Filename: "{app}\logview.exe"; Tasks: desktopicon

; "Otwórz w logview" per extension (SystemFileAssociations works whatever the
; default program of the extension is). On Windows 11 it is under
; "Pokaż więcej opcji".
[Registry]
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.log\shell\logview"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenIn}"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.log\shell\logview"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\logview.exe"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.log\shell\logview\command"; ValueType: string; ValueName: ""; ValueData: """{app}\logview.exe"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.txt\shell\logview"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenIn}"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.txt\shell\logview"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\logview.exe"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.txt\shell\logview\command"; ValueType: string; ValueName: ""; ValueData: """{app}\logview.exe"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.out\shell\logview"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenIn}"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.out\shell\logview"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\logview.exe"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.out\shell\logview\command"; ValueType: string; ValueName: ""; ValueData: """{app}\logview.exe"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.jsonl\shell\logview"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenIn}"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.jsonl\shell\logview"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\logview.exe"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.jsonl\shell\logview\command"; ValueType: string; ValueName: ""; ValueData: """{app}\logview.exe"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.gz\shell\logview"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenIn}"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.gz\shell\logview"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\logview.exe"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.gz\shell\logview\command"; ValueType: string; ValueName: ""; ValueData: """{app}\logview.exe"" ""%1"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.zst\shell\logview"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenIn}"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.zst\shell\logview"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\logview.exe"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\SystemFileAssociations\.zst\shell\logview\command"; ValueType: string; ValueName: ""; ValueData: """{app}\logview.exe"" ""%1"""; Tasks: contextmenu

[Run]
Filename: "{app}\logview.exe"; Description: "{cm:LaunchProgram,logview}"; Flags: nowait postinstall skipifsilent
