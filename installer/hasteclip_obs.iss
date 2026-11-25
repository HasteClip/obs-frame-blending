; The Windows installer for HasteClip Frame Blending, made by the release
; workflow (.github/workflows/release.yml) with Inno Setup 6.3 or later:
;
;   ISCC /DVersion=0.1.0 installer\hasteclip_obs.iss
;
; after `cargo build --release`. It puts the plugin where OBS Studio 30 and
; later look for plugins installed for everyone on the PC,
; C:\ProgramData\obs-studio\plugins\hasteclip_obs (its DLL in bin\64bit),
; which OBS's own updates leave alone.

#ifndef Version
  #define Version "0.0.0"
#endif

[Setup]
; The same in every release, so a new one replaces the one before.
AppId={{2A473BD4-E858-4B75-B56B-DFB181DFC9B2}
AppName=HasteClip Frame Blending
AppVersion={#Version}
AppVerName=HasteClip Frame Blending {#Version}
AppPublisher=HasteClip
AppPublisherURL=https://hasteclip.com/obs
AppSupportURL=https://github.com/HasteClip/hasteclip-obs/issues
AppUpdatesURL=https://hasteclip.com/obs
VersionInfoVersion={#Version}
DefaultDirName={commonappdata}\obs-studio\plugins\hasteclip_obs
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Windows 10 1903: Windows' capture, for Window and Display Capture.
MinVersion=10.0.18362
UninstallDisplayName=HasteClip Frame Blending (for OBS Studio)
OutputDir=..\dist
OutputBaseFilename=HasteClip-Frame-Blending-win-Setup
WizardStyle=modern
; OBS holds the plugin while it runs: the installer asks to close it first.
CloseApplications=yes
RestartApplications=no

[Files]
Source: "..\target\release\hasteclip_obs.dll"; DestDir: "{app}\bin\64bit"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Messages]
FinishedLabel=HasteClip Frame Blending is installed. If OBS Studio is open, start it again. Then right-click a Game Capture, Window Capture or Display Capture, choose Filters, click + and pick Frame Blending.
