; Inno Setup script for sc-watch.
;
;   ISCC.exe /DMyAppVersion=0.1.0 installer.iss
;
; Run by build.py --installer when Inno Setup is present, and by the GitHub
; Actions release workflow, whose Windows runner ships with it. It packages
; whatever PyInstaller left in dist\sc-watch, so build.py must run first.
;
; PER-USER, NOT PER-MACHINE
; -------------------------
; PrivilegesRequired=lowest installs under %LOCALAPPDATA%\Programs and needs no
; administrator prompt. That matters more than it sounds for a game utility:
; asking for elevation to install something that reads your screen is exactly
; the shape of thing people are right to refuse, and it buys nothing here. The
; app writes only to its own per-user data directory either way.
;
; WHAT AN UNINSTALL LEAVES
; ------------------------
; The program, all of it. Your data, none of it - the database holds
; reputation judgements you built up and the audit folder holds screenshots,
; and silently deleting those because someone uninstalled to reinstall a newer
; build would be unrecoverable. The uninstaller offers to remove them and
; defaults to no.

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif

#define MyAppName "sc-watch"
#define MyAppExeName "sc-watch.bat"
#define MyAppPublisher "sc-watch"
; Set by the release workflow from the repository it is building, so the
; support links in Add/Remove Programs point somewhere real.
#ifndef MyAppURL
  #define MyAppURL "https://github.com/sc-watch/sc-watch"
#endif

[Setup]
AppId={{7C3F1A64-9B2E-4F71-9E4B-5A2D8C1F0E33}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}/releases
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
; Shown only if a LICENSE file exists. Choosing one is the author's call, and
; an installer that refuses to build until they have is the wrong prompt at the
; wrong moment.
#if FileExists(AddBackslash(SourcePath) + "LICENSE")
LicenseFile=LICENSE
#endif
OutputDir=dist
OutputBaseFilename=sc-watch-{#MyAppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Refuse to install over a running copy rather than leaving half-replaced
; executables behind.
CloseApplications=yes
RestartApplications=no
UninstallDisplayName={#MyAppName} {#MyAppVersion}
UninstallDisplayIcon={app}\sc-watch.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; \
  GroupDescription: "Shortcuts:"

[Files]
; Everything the installer ships is in dist\sc-watch, which build.py fills:
; the executables, _internal, LICENSE, THIRD-PARTY-NOTICES.txt, README.md
; and docs/. One source directory, so this cannot reach back into a folder
; that exists on one machine and not on a CI runner.
Source: "dist\sc-watch\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; \
  WorkingDir: "{app}"; IconFilename: "{app}\sc-watch.exe"
Name: "{group}\sc-watch UI only"; Filename: "{app}\sc-watch-ui.exe"; \
  WorkingDir: "{app}"
Name: "{group}\Where is my data"; Filename: "{app}\sc-watch.exe"; \
  Parameters: "--where"; WorkingDir: "{app}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; \
  WorkingDir: "{app}"; IconFilename: "{app}\sc-watch.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Start sc-watch now"; \
  Flags: shellexec postinstall skipifsilent nowait

[UninstallDelete]
; PyInstaller's _internal is created wholesale by the installer, so removing
; the directory itself is safe. Nothing under {localappdata}\sc-watch is
; touched here; see the uninstall step below.
Type: filesandordirs; Name: "{app}\_internal"

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    DataDir := ExpandConstant('{localappdata}\sc-watch');
    if DirExists(DataDir) then
    begin
      // SuppressibleMsgBox, not MsgBox, and this is not a style preference.
      //
      // Plain MsgBox under /SUPPRESSMSGBOXES returns the AFFIRMATIVE and
      // ignores MB_DEFBUTTON2 entirely. Verified by installing 0.1.1 and
      // running a silent uninstall: it deleted the whole data directory
      // without asking anything. That is somebody's reputation database,
      // built up over months and impossible to reconstruct.
      //
      // SuppressibleMsgBox takes the suppressed answer as its last argument.
      // IDNO means an unattended uninstall NEVER deletes your data, which is
      // the only safe way round: keeping files nobody wanted costs disk, and
      // deleting files somebody wanted costs the whole point of the tool.
      if SuppressibleMsgBox('Also delete your sc-watch data?' + #13#10#13#10 +
                DataDir + #13#10#13#10 +
                'This is your contact database, your settings and your saved ' +
                'audit screenshots. Keep it if you are reinstalling or ' +
                'upgrading.',
                mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES then
        DelTree(DataDir, True, True, True);
    end;
  end;
end;
