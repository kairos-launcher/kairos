; Inno Setup script. Build: iscc /DAppVersion=0.1.0 packaging\kairos.iss
; Expects kairos.exe and plugins\ in dist\ (scripts/dist.sh, or see .github/workflows/release.yml).
#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{6B0E4C1A-2D63-4E5B-9F57-4C1B7A1E5D10}
AppName=Kairos
AppVersion={#AppVersion}
DefaultDirName={autopf}\Kairos
DefaultGroupName=Kairos
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=..\dist-installer
OutputBaseFilename=kairos-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName=Kairos
UninstallDisplayIcon={app}\kairos.exe
SetupIconFile=kairos.ico

[Tasks]
Name: "autostart"; Description: "Start Kairos when I sign in"
Name: "startmenu"; Description: "Create a Start Menu shortcut"
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[InstallDelete]
; Binaries from before the plugins were renamed to kairos-<name>.exe.
Type: files; Name: "{app}\plugins\calc\calc.exe"
Type: files; Name: "{app}\plugins\ai\ai.exe"

[Files]
Source: "..\dist\kairos.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\kairos.png"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\plugins\*"; DestDir: "{app}\plugins"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\Kairos"; Filename: "{app}\kairos.exe"; Tasks: startmenu
Name: "{autodesktop}\Kairos"; Filename: "{app}\kairos.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\kairos.exe"; Parameters: "--install-autostart"; Flags: runhidden; Tasks: autostart
Filename: "{app}\kairos.exe"; Description: "Launch Kairos now"; Flags: nowait postinstall skipifsilent
; Silent installs are the launcher updating itself: start it again, hidden.
Filename: "{app}\kairos.exe"; Parameters: "--background"; Flags: nowait runasoriginaluser; Check: WizardSilent

[UninstallRun]
Filename: "{app}\kairos.exe"; Parameters: "--quit"; Flags: runhidden; RunOnceId: "Quit"
Filename: "{app}\kairos.exe"; Parameters: "--uninstall-autostart"; Flags: runhidden; RunOnceId: "Autostart"
