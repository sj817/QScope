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
OutputBaseFilename=QScope-{#ProductVersion}-windows-x64-setup
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
; An online update runs setup silently with /RELAUNCH=1 and reopens QScope afterwards.
Filename: "{app}\QScope.exe"; Flags: nowait; Check: RelaunchAfterUpdate

[UninstallDelete]
Type: files; Name: "{app}\qscope-install.json"
Type: files; Name: "{app}\*.old"

[Code]
const
  WebViewKey = 'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';

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

function StopBackend(const Directory: String): Boolean;
var Code: Integer;
begin
  Result := True;
  if FileExists(Directory + '\qscope-cli.exe') then
    Result := Exec(Directory + '\qscope-cli.exe', 'service stop', Directory,
      SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var Code: Integer;
begin
  Result := '';
  if not StopBackend(ExpandConstant('{app}')) then begin
    Result := 'QScope 仍有活动研究任务。请先停止研究 QQ 和录制，然后重试。';
    Exit;
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
  Result := StopBackend(ExpandConstant('{app}'));
  if not Result then begin
    MsgBox('QScope 仍有活动研究任务。请先停止研究 QQ 和录制。', mbError, MB_OK);
    Exit;
  end;
  if FileExists(ExpandConstant('{app}\qscope-cli.exe')) then
    Result := Exec(ExpandConstant('{app}\qscope-cli.exe'), 'cli uninstall --if-owned',
      ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;
