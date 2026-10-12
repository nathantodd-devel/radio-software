; The Windows installer, built with Inno Setup 6 (https://jrsoftware.org/isinfo.php):
;
;   iscc /DAppVersion=1.0.0 /DSourceDir=..\dist\dmps-v1.0.0-windows-x86_64 installer\dmps.iss
;
; SourceDir is an unpacked release package: the programs, the DLLs they load,
; the web page and the README. The release workflow builds this for every
; release.
;
; It installs for everyone on the computer (asking for administrator rights)
; or for the current user alone (asking for none), as chosen when it starts,
; and can set up a scheduled task that keeps the programs up to date:
;
;   for everyone     the task runs as SYSTEM, which may write to Program Files
;   for one user     the task runs as that user, who owns the files
;
; A scheduled task rather than a Windows service because installing a
; service takes administrator rights, which an install for one user doesn't
; have, and because all it has to do is run something now and then.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\dist\package"
#endif
#define AppName "Digital Mobile Police Scanner"
#define ShortName "DMPS"
; What the updater task is called: one for the computer, or one per user.
#define SystemTask "DMPS\Updater"
#define UserTask "DMPS Updater for {username}"
; How often the task looks for a new release.
#define CheckEveryHours "6"

[Setup]
; Identifies the program to Windows across versions: never change it.
AppId={{6F0B9A5E-3C1D-4E7A-9B52-D4A8C1F0E2B7}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Fairhouse
AppPublisherURL=https://github.com/nathantodd-devel/radio-software
AppSupportURL=https://github.com/nathantodd-devel/radio-software/issues
AppUpdatesURL=https://github.com/nathantodd-devel/radio-software/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\{#ShortName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE
SetupIconFile=..\icons\dmps.ico
UninstallDisplayIcon={app}\scanner-ui.exe
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Without administrator rights unless the person installing chooses "for all
; users" in the dialog shown at the start (or passes /ALLUSERS).
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
; The programs can be replaced while they run, so there is no need to make
; anyone close them; they pick the new version up when restarted.
CloseApplications=no
OutputDir=..\dist
OutputBaseFilename=dmps-v{#AppVersion}-windows-x86_64-setup

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autoupdate"; Description: "Keep {#ShortName} up to date automatically (checks GitHub every {#CheckEveryHours} hours)"; GroupDescription: "Updates:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\scanner-ui.exe"
Name: "{group}\{#ShortName} web server"; Filename: "{app}\scanner-web.exe"; WorkingDir: "{app}"
Name: "{group}\Read me"; Filename: "{app}\README.txt"
Name: "{group}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\scanner-ui.exe"; Tasks: desktopicon

[Run]
; The updater task. scanner-ui is the program run because it has no console
; window to flash up; with this option it updates and exits without showing
; anything. Installing again replaces the task (/F).
Filename: "{sys}\schtasks.exe"; \
    Parameters: "/Create /F /TN ""{#SystemTask}"" /TR ""\""{app}\scanner-ui.exe\"" --update-quietly"" /SC HOURLY /MO {#CheckEveryHours} /RU SYSTEM /RL HIGHEST"; \
    Flags: runhidden; Tasks: autoupdate; Check: IsAdminInstallMode; StatusMsg: "Setting up automatic updates..."
Filename: "{sys}\schtasks.exe"; \
    Parameters: "/Create /F /TN ""{#UserTask}"" /TR ""\""{app}\scanner-ui.exe\"" --update-quietly"" /SC HOURLY /MO {#CheckEveryHours}"; \
    Flags: runhidden; Tasks: autoupdate; Check: not IsAdminInstallMode; StatusMsg: "Setting up automatic updates..."
Filename: "{app}\scanner-ui.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\schtasks.exe"; Parameters: "/Delete /F /TN ""{#SystemTask}"""; Flags: runhidden; Check: IsAdminInstallMode; RunOnceId: "RemoveSystemUpdater"
Filename: "{sys}\schtasks.exe"; Parameters: "/Delete /F /TN ""{#UserTask}"""; Flags: runhidden; Check: not IsAdminInstallMode; RunOnceId: "RemoveUserUpdater"

[UninstallDelete]
; What updates leave that the installer didn't put there: newer copies of
; the web page (its files are named after their contents), programs moved
; aside while they were running, and the updater's log.
Type: filesandordirs; Name: "{app}\web"
Type: files; Name: "{app}\*.old"
Type: files; Name: "{app}\update.log"

[Code]
// Choosing not to update automatically on a later install takes away the
// task an earlier one set up.
procedure CurStepChanged(CurStep: TSetupStep);
var
  Task: String;
  ResultCode: Integer;
begin
  if (CurStep = ssPostInstall) and not WizardIsTaskSelected('autoupdate') then
  begin
    if IsAdminInstallMode then
      Task := '{#SystemTask}'
    else
      Task := ExpandConstant('{#UserTask}');
    Exec(ExpandConstant('{sys}\schtasks.exe'), '/Delete /F /TN "' + Task + '"', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  end;
end;
