#ifndef ProductVersion
  #error ProductVersion is required
#endif

[Setup]
AppId=io.github.sj817.qscope
AppName=QScope
AppVersion={#ProductVersion}
AppPublisher=sj817
AppPublisherURL=https://github.com/sj817/QScope
AppSupportURL=https://github.com/sj817/QScope/issues
DefaultDirName={localappdata}\Programs\QScope
DefaultGroupName=QScope
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
WizardStyle=modern dynamic windows11
WizardSizePercent=110
DisableWelcomePage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
ChangesEnvironment=yes
CloseApplications=yes
CloseApplicationsFilter=QScope.exe
RestartApplications=no
UninstallFilesDir={localappdata}\QScope\maintenance
UninstallDisplayIcon={app}\QScope.exe
SetupIconFile={#Repository}\apps\desktop\icons\icon.ico
OutputDir={#OutputDirectory}
OutputBaseFilename=QScope-windows-x64-setup
Compression=lzma2/ultra64
SolidCompression=yes

[Languages]
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "globalcli"; Description: "启用全局 qs 命令"
Name: "desktopicon"; Description: "创建桌面快捷方式"; Flags: unchecked

[Files]
Source: "{#ProductDirectory}\QScope.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ProductDirectory}\qscope-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#WebViewBootstrapper}"; DestName: "MicrosoftEdgeWebview2Setup.exe"; Flags: dontcopy

[Icons]
Name: "{group}\QScope"; Filename: "{app}\QScope.exe"
Name: "{autodesktop}\QScope"; Filename: "{app}\QScope.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\QScope.exe"; Description: "打开 QScope"; Flags: nowait postinstall skipifsilent
; 在线更新通过 /RELAUNCH=1 静默安装，完成后重新打开 QScope。
Filename: "{app}\QScope.exe"; Flags: nowait; Check: RelaunchAfterUpdate

[UninstallDelete]
Type: files; Name: "{app}\qscope-install.json"
Type: files; Name: "{app}\*.old"

[Code]
const
  WebViewKey = 'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';

{ The Browse button opens the Windows folder picker instead of the classic folder tree. }
const
  FileOpenDialogClass = '{DC1C5A9C-E88A-4DDE-A5A1-60F82A20AEF7}';
  ShellItemInterface = '{43826D1E-E718-42EE-BC55-A1E261C37BFE}';
  FOS_PICKFOLDERS = $20;
  FOS_FORCEFILESYSTEM = $40;
  FOS_PATHMUSTEXIST = $800;
  SIGDN_FILESYSPATH = $80058000;

type
  IShellItem = interface(IUnknown)
    '{43826D1E-E718-42EE-BC55-A1E261C37BFE}'
    procedure BindToHandler;
    procedure GetParent;
    function GetDisplayName(Form: DWORD; var Name: NativeInt): HResult;
  end;

  { Methods in vtable order; unused ones only hold their slot. }
  IFileDialog = interface(IUnknown)
    '{42F85136-DB7E-439C-85F1-E4075D135FC8}'
    function Show(Owner: HWND): HResult;
    procedure SetFileTypes;
    procedure SetFileTypeIndex;
    procedure GetFileTypeIndex;
    procedure Advise;
    procedure Unadvise;
    function SetOptions(Options: DWORD): HResult;
    function GetOptions(var Options: DWORD): HResult;
    procedure SetDefaultFolder;
    function SetFolder(Item: IShellItem): HResult;
    procedure GetFolder;
    procedure GetCurrentSelection;
    procedure SetFileName;
    procedure GetFileName;
    function SetTitle(Title: String): HResult;
    function SetOkButtonLabel(Text: String): HResult;
    procedure SetFileNameLabel;
    function GetResult(var Item: IShellItem): HResult;
  end;

function SHCreateItemFromParsingName(Path: String; Context: NativeInt;
  var Kind: TGUID; var Item: IShellItem): HResult;
external 'SHCreateItemFromParsingName@shell32.dll stdcall';
function lstrlenW(Text: NativeInt): Integer;
external 'lstrlenW@kernel32.dll stdcall';
function lstrcpynW(Target: String; Source: NativeInt; Length: Integer): NativeInt;
external 'lstrcpynW@kernel32.dll stdcall';
procedure CoTaskMemFree(Memory: NativeInt);
external 'CoTaskMemFree@ole32.dll stdcall';

function ItemPath(Item: IShellItem; var Path: String): Boolean;
var Name: NativeInt; Length: Integer;
begin
  Result := False;
  Name := 0;
  if (Item.GetDisplayName(SIGDN_FILESYSPATH, Name) <> 0) or (Name = 0) then
    Exit;
  try
    Length := lstrlenW(Name);
    SetLength(Path, Length);
    lstrcpynW(Path, Name, Length + 1);
    Result := Length > 0;
  finally
    CoTaskMemFree(Name);
  end;
end;

{ Returns False when the picker was cancelled; raises when the picker is unavailable. }
function PickFolder(Owner: HWND; const Title: String; var Directory: String): Boolean;
var
  Dialog: IFileDialog;
  Start, Picked: IShellItem;
  Kind: TGUID;
  Options: DWORD;
  Folder: String;
begin
  Result := False;
  Dialog := IFileDialog(CreateComObject(StringToGuid(FileOpenDialogClass)));
  Options := 0;
  Dialog.GetOptions(Options);
  Dialog.SetOptions(Options or FOS_PICKFOLDERS or FOS_FORCEFILESYSTEM or FOS_PATHMUSTEXIST);
  Dialog.SetTitle(Title);
  { Start in the deepest existing folder of the current path. }
  Folder := RemoveBackslashUnlessRoot(Directory);
  while (Folder <> '') and not DirExists(Folder) and (ExtractFileDir(Folder) <> Folder) do
    Folder := ExtractFileDir(Folder);
  Kind := StringToGuid(ShellItemInterface);
  if DirExists(Folder) and (SHCreateItemFromParsingName(Folder, 0, Kind, Start) = 0) then
    Dialog.SetFolder(Start);
  if Dialog.Show(Owner) <> 0 then
    Exit;
  if (Dialog.GetResult(Picked) = 0) and ItemPath(Picked, Folder) then begin
    Directory := Folder;
    Result := True;
  end;
end;

function RuntimeVersion(Root: Integer): Boolean;
var Version: String;
begin
  Result := RegQueryStringValue(Root, WebViewKey, 'pv', Version) and
    (Version <> '') and (Version <> '0.0.0.0');
end;

function HasWebView: Boolean;
begin
  Result := RuntimeVersion(HKCU32) or RuntimeVersion(HKCU64) or
    RuntimeVersion(HKLM32) or RuntimeVersion(HKLM64);
end;

function StopBackend(const Directory: String; CloseTasks: Boolean): Boolean;
var Code: Integer; Arguments: String;
begin
  Result := True;
  Arguments := 'service stop';
  if CloseTasks then
    Arguments := Arguments + ' --close-tasks';
  if FileExists(Directory + '\qscope-cli.exe') then
    Result := Exec(Directory + '\qscope-cli.exe', Arguments, Directory,
      SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var Code: Integer;
begin
  Result := '';
  if not StopBackend(ExpandConstant('{app}'), False) then begin
    Result := 'QScope 仍有活动研究任务。请先停止研究 QQ 和录制，然后重试。';
    { An online update has closed its tasks already; a setup run by hand asks first. }
    if WizardSilent then
      Exit;
    if MsgBox('QScope 仍有研究 QQ 或录制在运行。关闭它们并继续安装吗？' + #13#10 +
      '录制会先保存，抓包会写入结束标记。', mbConfirmation, MB_YESNO) <> IDYES then
      Exit;
    if not StopBackend(ExpandConstant('{app}'), True) then
      Exit;
    Result := '';
  end;
  if not HasWebView then begin
    ExtractTemporaryFile('MicrosoftEdgeWebview2Setup.exe');
    if not Exec(ExpandConstant('{tmp}\MicrosoftEdgeWebview2Setup.exe'),
      '/silent /install', '', SW_HIDE, ewWaitUntilTerminated, Code) or
      ((Code <> 0) and (Code <> 3010)) or not HasWebView then
      Result := 'WebView2 准备失败。请连接网络后重试。';
    NeedsRestart := Code = 3010;
  end;
end;

function RelaunchAfterUpdate: Boolean;
begin
  Result := WizardSilent and (ExpandConstant('{param:relaunch|0}') = '1');
end;

procedure CurStepChanged(CurStep: TSetupStep);
var Code: Integer;
begin
  { Marks this copy as installed, so online updates use the setup package. }
  if CurStep = ssPostInstall then
    SaveStringToFile(ExpandConstant('{app}\qscope-install.json'),
      '{"schema_version":1,"kind":"installer"}', False);
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('globalcli') then
    if not Exec(ExpandConstant('{app}\qscope-cli.exe'), 'cli install',
      ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
      RaiseException('全局 qs 命令注册失败。请运行 qscope-cli cli install 查看详情。');
end;

function InitializeUninstall: Boolean;
var Code: Integer;
begin
  Result := StopBackend(ExpandConstant('{app}'), False);
  if not Result then begin
    MsgBox('QScope 仍有活动研究任务。请先停止研究 QQ 和录制。', mbError, MB_OK);
    Exit;
  end;
  if FileExists(ExpandConstant('{app}\qscope-cli.exe')) then
    Result := Exec(ExpandConstant('{app}\qscope-cli.exe'), 'cli uninstall --if-owned',
      ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

procedure BrowseDirectory(Sender: TObject);
var Directory: String; Picked: Boolean;
begin
  Directory := WizardForm.DirEdit.Text;
  try
    Picked := PickFolder(WizardForm.Handle, SetupMessage(msgWizardSelectDir), Directory);
  except
    { Without the Windows folder picker, the classic folder tree. }
    Picked := BrowseForFolder(SetupMessage(msgBrowseDialogLabel), Directory, False);
  end;
  if not Picked then
    Exit;
  { Like the classic dialog, a picked folder gets its own QScope subfolder. }
  if CompareText(ExtractFileName(RemoveBackslashUnlessRoot(Directory)), 'QScope') <> 0 then
    Directory := AddBackslash(Directory) + 'QScope';
  WizardForm.DirEdit.Text := Directory;
end;

procedure InitializeWizard;
begin
  WizardForm.DirBrowseButton.OnClick := @BrowseDirectory;
end;
