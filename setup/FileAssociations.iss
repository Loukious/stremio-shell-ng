; Shared by the x64 and ARM64 per-user installers.
#define AssocCapabilities "Software\Stremio5\Capabilities"
#define AssocTorrentKey "Stremio5.Torrent"
#define AssocStremioKey "Stremio5.Url"
#define AssocMagnetKey "Stremio5.Magnet"

[Registry]
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "Stremio5"; ValueData: "{#AssocCapabilities}"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "{#AssocCapabilities}"; ValueType: string; ValueName: "ApplicationName"; ValueData: "Stremio"; Flags: uninsdeletekey
Root: HKCU; Subkey: "{#AssocCapabilities}"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Stremio links, torrent files and magnet links"
Root: HKCU; Subkey: "{#AssocCapabilities}"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: "{app}\{#MyAppExeName},0"
Root: HKCU; Subkey: "{#AssocCapabilities}\URLAssociations"; ValueType: string; ValueName: "stremio"; ValueData: "{#AssocStremioKey}"
Root: HKCU; Subkey: "{#AssocCapabilities}\URLAssociations"; ValueType: string; ValueName: "magnet"; ValueData: "{#AssocMagnetKey}"; Tasks: assoctorrent
Root: HKCU; Subkey: "{#AssocCapabilities}\FileAssociations"; ValueType: string; ValueName: ".torrent"; ValueData: "{#AssocTorrentKey}"; Tasks: assoctorrent

Root: HKCU; Subkey: "Software\Classes\{#AssocStremioKey}"; ValueType: string; ValueName: ""; ValueData: "URL:Stremio Protocol"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\{#AssocStremioKey}"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\{#AssocStremioKey}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#MyAppExeName},0"
Root: HKCU; Subkey: "Software\Classes\{#AssocStremioKey}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%1"""

Root: HKCU; Subkey: "Software\Classes\{#AssocMagnetKey}"; ValueType: string; ValueName: ""; ValueData: "URL:BitTorrent magnet"; Flags: uninsdeletekey; Tasks: assoctorrent
Root: HKCU; Subkey: "Software\Classes\{#AssocMagnetKey}"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""; Tasks: assoctorrent
Root: HKCU; Subkey: "Software\Classes\{#AssocMagnetKey}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#MyAppExeName},0"; Tasks: assoctorrent
Root: HKCU; Subkey: "Software\Classes\{#AssocMagnetKey}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%1"""; Tasks: assoctorrent

Root: HKCU; Subkey: "Software\Classes\.torrent\OpenWithProgids"; ValueType: string; ValueName: "{#AssocTorrentKey}"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoctorrent
Root: HKCU; Subkey: "Software\Classes\{#AssocTorrentKey}"; ValueType: string; ValueName: ""; ValueData: "BitTorrent file"; Flags: uninsdeletekey; Tasks: assoctorrent
Root: HKCU; Subkey: "Software\Classes\{#AssocTorrentKey}\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#MyAppExeName},0"; Tasks: assoctorrent
Root: HKCU; Subkey: "Software\Classes\{#AssocTorrentKey}\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%1"""; Tasks: assoctorrent
Root: HKCU; Subkey: "Software\Classes\Applications\{#MyAppExeName}\SupportedTypes"; ValueType: string; ValueName: ".torrent"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoctorrent

[Code]
var
  TorrentWasRegistered: Boolean;
  StremioProtocolBackup, MagnetProtocolBackup: String;

function OwnsClass(const Name: String): Boolean;
var
  Command: String;
begin
  Result := RegQueryStringValue(HKCU, 'Software\Classes\' + Name + '\shell\open\command', '', Command) and
    (CompareText(Command, ExpandConstant('"{app}\{#MyAppExeName}" "%1"')) = 0);
end;

procedure RemoveOwnedClass(const Name: String);
begin
  if OwnsClass(Name) then
    RegDeleteKeyIncludingSubkeys(HKCU, 'Software\Classes\' + Name);
end;

procedure RegisterProtocol(const Scheme, Description: String);
var
  Key: String;
begin
  { Existing defaults belong to the user. Do not replace another handler. }
  if RegKeyExists(HKCR, Scheme) and not OwnsClass(Scheme) then
    Exit;
  Key := 'Software\Classes\' + Scheme;
  RegWriteStringValue(HKCU, Key, '', Description);
  RegWriteStringValue(HKCU, Key, 'URL Protocol', '');
  RegWriteStringValue(HKCU, Key + '\DefaultIcon', '', ExpandConstant('{app}\{#MyAppExeName},0'));
  RegWriteStringValue(HKCU, Key + '\shell\open\command', '', ExpandConstant('"{app}\{#MyAppExeName}" "%1"'));
end;

procedure UpdateFileAssociations;
begin
  { Remove the malformed OpenWith entry written by older installers. }
  RegDeleteValue(HKCU, 'Software\Classes\.torrent}\OpenWithProgids', 'Stremio.torrent');
  RegDeleteKeyIfEmpty(HKCU, 'Software\Classes\.torrent}\OpenWithProgids');
  RegDeleteKeyIfEmpty(HKCU, 'Software\Classes\.torrent}');
  RegisterProtocol('stremio', 'URL:Stremio Protocol');
  if WizardIsTaskSelected('assoctorrent') then
    RegisterProtocol('magnet', 'URL:BitTorrent magnet')
  else begin
    { Keep the legacy ProgID while enabled: an existing default can use it. }
    RemoveOwnedClass('Stremio.torrent');
    RemoveOwnedClass('magnet');
    RemoveOwnedClass('{#AssocMagnetKey}');
    RemoveOwnedClass('{#AssocTorrentKey}');
    RegDeleteValue(HKCU, '{#AssocCapabilities}\URLAssociations', 'magnet');
    RegDeleteValue(HKCU, '{#AssocCapabilities}\FileAssociations', '.torrent');
    RegDeleteValue(HKCU, 'Software\Classes\.torrent\OpenWithProgids', '{#AssocTorrentKey}');
    RegDeleteValue(HKCU, 'Software\Classes\Applications\{#MyAppExeName}\SupportedTypes', '.torrent');
  end;
end;

procedure OpenDefaultApps;
var
  Version: TWindowsVersion;
  SettingsURL: String;
  ErrorCode: Integer;
begin
  if WizardSilent or TorrentWasRegistered or not WizardIsTaskSelected('assoctorrent') then
    Exit;
  SettingsURL := 'ms-settings:defaultapps';
  GetWindowsVersionEx(Version);
  if (Version.Major >= 10) and (Version.Build >= 22000) then
    SettingsURL := SettingsURL + '?registeredAppUser=Stremio5';
  ShellExecAsOriginalUser('', SettingsURL, '', '', SW_SHOW, ewNoWait, ErrorCode);
end;

function BackupForeignProtocol(const Scheme: String; var Backup: String): Boolean;
var
  ErrorCode: Integer;
begin
  Result := True;
  if RegKeyExists(HKCU, 'Software\Classes\' + Scheme) and not OwnsClass(Scheme) then begin
    Backup := ExpandConstant('{tmp}\stremio-' + Scheme + '.reg');
    Result := Exec(ExpandConstant('{sys}\reg.exe'),
      'export "HKEY_CURRENT_USER\Software\Classes\' + Scheme + '" "' + Backup + '" /y',
      '', SW_HIDE, ewWaitUntilTerminated, ErrorCode) and (ErrorCode = 0);
  end;
end;

function InitializeUninstall: Boolean;
begin
  { Upgrades retain old uninstall-log entries that delete these shared keys.
    Preserve a replacement handler before those older entries can run. }
  Result := BackupForeignProtocol('stremio', StremioProtocolBackup) and
    BackupForeignProtocol('magnet', MagnetProtocolBackup);
  if not Result then
    SuppressibleMsgBox('Unable to preserve the current link handlers. Uninstall has been cancelled.', mbError, MB_OK, IDOK);
end;

procedure RestoreProtocol(const Backup: String);
var
  ErrorCode: Integer;
begin
  if Backup <> '' then
    if not Exec(ExpandConstant('{sys}\reg.exe'), 'import "' + Backup + '"',
      '', SW_HIDE, ewWaitUntilTerminated, ErrorCode) or (ErrorCode <> 0) then
      SuppressibleMsgBox('Unable to restore link handlers from ' + Backup, mbError, MB_OK, IDOK);
end;
