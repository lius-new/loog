; Loog per-user installer, modelled on Zed's Inno Setup installer
; (crates/zed/resources/windows/zed.iss in zed-industries/zed).
;
; Built by `node scripts/package/package.mjs bundle`, which passes:
;   Version, VersionQuad, StageDir, OutputDir, OutputBase, AppIcon, Homepage
;
; Installs into %LOCALAPPDATA%\Programs\Loog without elevation. User data
; (sessions, settings) is never removed.

#define AppName "Loog"
#define AppExeName "Loog.exe"
#define AppLogo AddBackslash(ExtractFileDir(AppIcon)) + "source\app-icon-transparent-256.png"

[Setup]
; Never change the AppId: it identifies the installation across upgrades.
AppId={{E00707BB-1779-4DA4-ADCB-7604AB62244A}
AppName={#AppName}
AppVersion={#Version}
AppVerName={#AppName} {#Version}
AppPublisher={#AppName}
AppPublisherURL={#Homepage}
AppSupportURL={#Homepage}
AppUpdatesURL={#Homepage}
VersionInfoVersion={#VersionQuad}
VersionInfoProductName={#AppName}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
; Keep the interactive entry point and destination visible, including upgrades.
DisableWelcomePage=no
DisableDirPage=no
DisableProgramGroupPage=yes
DisableReadyPage=yes
PrivilegesRequired=lowest
; This per-user installer does not elevate. On affected Windows builds,
; RedirectionGuard reaches applications launched by [Run] and their shells,
; blocking user-created junctions such as Scoop's apps\fnm\current.
RedirectionGuard=no
SetupArchitecture=x64
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
WizardStyle=modern dynamic
ShowLanguageDialog=no
SetupIconFile={#AppIcon}
WizardImageFile={#AppLogo}
WizardSmallImageFile={#AppLogo}
UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppName}
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBase}
Compression=lzma2
SolidCompression=yes
; Offer to close a running Loog before its files are replaced.
CloseApplications=yes
RestartApplications=no
ChangesEnvironment=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Messages]
english.WelcomeLabel1=Welcome to the [name] installer
english.WelcomeLabel2=This will install [name/ver] for the current user.
english.WizardSelectTasks=Installation options
english.SelectTasksDesc=Choose shortcuts and Windows integration.
english.SelectTasksLabel2=Choose the options you want for [name], then click Install.

[CustomMessages]
english.DesktopIcon=Create a desktop shortcut
english.StartMenuIcon=Create a Start menu shortcut
english.Shortcuts=Shortcuts:
english.WindowsIntegration=Windows integration:
english.AddContextMenuFiles=Add "Open with %1" to file context menus
english.AddContextMenuFolders=Add "Open with %1" to folder context menus
english.AddToPath=Add to PATH (requires shell restart)
english.OpenWith=Open with %1

[Tasks]
Name: "desktopicon"; Description: "{cm:DesktopIcon}"; GroupDescription: "{cm:Shortcuts}"
Name: "startmenuicon"; Description: "{cm:StartMenuIcon}"; GroupDescription: "{cm:Shortcuts}"
Name: "addcontextmenufiles"; Description: "{cm:AddContextMenuFiles,{#AppName}}"; GroupDescription: "{cm:WindowsIntegration}"
Name: "addcontextmenufolders"; Description: "{cm:AddContextMenuFolders,{#AppName}}"; GroupDescription: "{cm:WindowsIntegration}"
Name: "addtopath"; Description: "{cm:AddToPath}"; GroupDescription: "{cm:WindowsIntegration}"

[InstallDelete]
; Replace the managed Git runtime wholesale so files dropped by a newer
; MinGit never linger next to the new ones.
Type: filesandordirs; Name: "{app}\runtime"
; Shortcuts follow this run's choices on upgrade.
Type: files; Name: "{autodesktop}\{#AppName}.lnk"; Tasks: not desktopicon
Type: files; Name: "{autoprograms}\{#AppName}.lnk"; Tasks: not startmenuicon

[Files]
Source: "{#StageDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: startmenuicon
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; Tasks: desktopicon

[Registry]
; "Open with Loog" for files.
Root: HKCU; Subkey: "Software\Classes\*\shell\{#AppName}"; ValueType: none; Flags: deletekey; Tasks: not addcontextmenufiles
Root: HKCU; Subkey: "Software\Classes\*\shell\{#AppName}"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWith,{#AppName}}"; Flags: uninsdeletekey; Tasks: addcontextmenufiles
Root: HKCU; Subkey: "Software\Classes\*\shell\{#AppName}"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExeName}"; Tasks: addcontextmenufiles
Root: HKCU; Subkey: "Software\Classes\*\shell\{#AppName}\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"" ""%1"""; Tasks: addcontextmenufiles

; "Open with Loog" for folders, folder backgrounds and drives.
Root: HKCU; Subkey: "Software\Classes\Directory\shell\{#AppName}"; ValueType: none; Flags: deletekey; Tasks: not addcontextmenufolders
Root: HKCU; Subkey: "Software\Classes\Directory\shell\{#AppName}"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWith,{#AppName}}"; Flags: uninsdeletekey; Tasks: addcontextmenufolders
Root: HKCU; Subkey: "Software\Classes\Directory\shell\{#AppName}"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExeName}"; Tasks: addcontextmenufolders
Root: HKCU; Subkey: "Software\Classes\Directory\shell\{#AppName}\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"" ""%1"""; Tasks: addcontextmenufolders
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\{#AppName}"; ValueType: none; Flags: deletekey; Tasks: not addcontextmenufolders
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\{#AppName}"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWith,{#AppName}}"; Flags: uninsdeletekey; Tasks: addcontextmenufolders
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\{#AppName}"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExeName}"; Tasks: addcontextmenufolders
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\{#AppName}\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"" ""%V"""; Tasks: addcontextmenufolders
Root: HKCU; Subkey: "Software\Classes\Drive\shell\{#AppName}"; ValueType: none; Flags: deletekey; Tasks: not addcontextmenufolders
Root: HKCU; Subkey: "Software\Classes\Drive\shell\{#AppName}"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWith,{#AppName}}"; Flags: uninsdeletekey; Tasks: addcontextmenufolders
Root: HKCU; Subkey: "Software\Classes\Drive\shell\{#AppName}"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\{#AppExeName}"; Tasks: addcontextmenufolders
Root: HKCU; Subkey: "Software\Classes\Drive\shell\{#AppName}\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExeName}"" ""%V"""; Tasks: addcontextmenufolders

[Run]
Filename: "{app}\{#AppExeName}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; Loog's in-app update runs Setup with /VERYSILENT /relaunch=1 after it quits.
Filename: "{app}\{#AppExeName}"; Flags: nowait; Check: RelaunchRequested

[Code]
const
  EnvironmentKey = 'Environment';

procedure LayoutWizardLogo(Image: TBitmapImage; Page: TNewNotebookPage);
var
  AreaWidth, LogoSize: Integer;
begin
  { Keep the square app icon centered and proportional at the current DPI. }
  AreaWidth := Image.Width;
  LogoSize := AreaWidth * 3 div 4;
  Image.SetBounds((AreaWidth - LogoSize) div 2,
    (Page.ClientHeight - LogoSize) div 2, LogoSize, LogoSize);
end;

procedure InitializeWizard;
begin
  LayoutWizardLogo(WizardForm.WizardBitmapImage, WizardForm.WelcomePage);
  LayoutWizardLogo(WizardForm.WizardBitmapImage2, WizardForm.FinishedPage);
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  { The options page starts installation directly because the ready page is hidden. }
  if CurPageID = wpSelectTasks then
    WizardForm.NextButton.Caption := SetupMessage(msgButtonInstall);
end;

function RelaunchRequested: Boolean;
begin
  Result := WizardSilent and (ExpandConstant('{param:relaunch|0}') = '1');
end;

function BinDir: String;
begin
  Result := ExpandConstant('{app}\bin');
end;

function SamePathEntry(Entry, Dir: String): Boolean;
begin
  Result := CompareText(RemoveBackslashUnlessRoot(Trim(Entry)), RemoveBackslashUnlessRoot(Dir)) = 0;
end;

{ Return Path without any entry equal to Dir; every other entry is kept as is. }
function WithoutPathEntry(Path, Dir: String): String;
var
  Rest, Entry: String;
  Separator: Integer;
begin
  Result := '';
  Rest := Path;
  while Rest <> '' do
  begin
    Separator := Pos(';', Rest);
    if Separator = 0 then
    begin
      Entry := Rest;
      Rest := '';
    end
    else
    begin
      Entry := Copy(Rest, 1, Separator - 1);
      Delete(Rest, 1, Separator);
    end;
    if not SamePathEntry(Entry, Dir) then
    begin
      if Result <> '' then
        Result := Result + ';';
      Result := Result + Entry;
    end;
  end;
end;

{ Add or remove Dir in the user's PATH, leaving it untouched when nothing changes. }
procedure UpdateUserPath(Dir: String; Add: Boolean);
var
  OldPath, NewPath: String;
begin
  if not RegQueryStringValue(HKCU, EnvironmentKey, 'Path', OldPath) then
    OldPath := '';
  NewPath := WithoutPathEntry(OldPath, Dir);
  if Add then
  begin
    if (NewPath <> '') and (NewPath[Length(NewPath)] <> ';') then
      NewPath := NewPath + ';';
    NewPath := NewPath + Dir;
  end;
  if NewPath <> OldPath then
    RegWriteExpandStringValue(HKCU, EnvironmentKey, 'Path', NewPath);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    UpdateUserPath(BinDir, WizardIsTaskSelected('addtopath'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    UpdateUserPath(BinDir, False);
end;
