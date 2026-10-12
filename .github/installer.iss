[Setup]
AppId=interpolini
AppName=interpolini
; without a version, which the updater of the app cannot change
AppVerName=interpolini
AppPublisher=z1xus
AppPublisherURL=https://github.com/Z1xus/interpolini
AppMutex=interpolini
DefaultDirName={localappdata}\Programs\interpolini
PrivilegesRequired=lowest
ArchitecturesAllowed=x64os
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\interpolini.exe
SetupIconFile=..\assets\interpolini.ico
WizardStyle=modern
OutputDir={#output}
OutputBaseFilename={#name}-setup
Compression=lzma2/max
SolidCompression=yes
TouchDate=1980-01-01
TouchTime=00:00

[Tasks]
Name: startmenu; Description: "Start menu shortcut"
Name: sendto; Description: "Send to menu"
Name: desktop; Description: "Desktop shortcut"; Flags: unchecked

[Files]
Source: "{#output}\{#name}\*"; DestDir: "{app}"; Flags: recursesubdirs ignoreversion touch

[Icons]
Name: "{userprograms}\interpolini"; Filename: "{app}\interpolini.exe"; Tasks: startmenu
Name: "{usersendto}\interpolini"; Filename: "{app}\interpolini-cli.exe"; IconFilename: "{app}\interpolini.exe"; Tasks: sendto
Name: "{userdesktop}\interpolini"; Filename: "{app}\interpolini.exe"; Tasks: desktop

[InstallDelete]
Type: files; Name: "{userprograms}\interpolini.lnk"; Tasks: not startmenu
Type: files; Name: "{usersendto}\interpolini.lnk"; Tasks: not sendto
Type: files; Name: "{userdesktop}\interpolini.lnk"; Tasks: not desktop

[Run]
Filename: "{app}\interpolini.exe"; Description: "{cm:LaunchProgram,interpolini}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
function Empty(Folder: String): Boolean;
var
  Found: TFindRec;
begin
  Result := True;
  if FindFirst(Folder + '\*', Found) then
    try
      repeat
        Result := (Found.Name = '.') or (Found.Name = '..');
      until not Result or not FindNext(Found);
    finally
      FindClose(Found);
    end;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := (CurPageID <> wpSelectDir) or FileExists(WizardDirValue + '\interpolini.exe') or Empty(WizardDirValue);
  if not Result then
    SuppressibleMsgBox('Choose an empty folder.', mbError, MB_OK, IDOK);
end;
