; 词典的 Windows 安装程序（Inno Setup 6）。
;
;   ISCC.exe tools\installer.iss /DAppVersion=0.1.2 /DPayload=<装好的目录>
;
; 装到 {localappdata}\Programs 而不是 Program Files：后者要管理员权限，
; 为一个词典弹 UAC 不值当。而且程序把 settings.json 写在自己旁边 ——
; 装进 Program Files 的话那个文件根本写不进去。

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef Payload
  #define Payload "..\dist\payload"
#endif

[Setup]
; 这个 GUID 认的是「同一个程序」，升级时靠它找到旧版本原地覆盖。**不要改。**
AppId={{7A3E1C64-9B2D-4F58-8E11-6C0D2A5F41B7}
AppName=词典
AppVerName=词典 {#AppVersion}
AppVersion={#AppVersion}
AppPublisher=xchencehn
AppSupportURL=https://github.com/xchencehn/yinyidict
DefaultDirName={localappdata}\Programs\yinyidict
DefaultGroupName=词典
; 不问装到哪：这个程序没有「装到别处」的正当理由，少一页是一页
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=yinyidict-{#AppVersion}-windows-x64-setup
; 安装向导自己的图标。build.rs 把 .ico 画在 OUT_DIR 里，由 package.ps1
; 捞出来放到 dist\dict.ico —— 不往版本库里塞二进制资源。
#ifdef SetupIcon
SetupIconFile={#SetupIcon}
#endif
UninstallDisplayIcon={app}\dict.exe
; 装的东西里大头是 311 MB 的模型和 415 MB 的索引，本来就压不动多少，
; 用 normal 而不是 max —— max 能多省的那点体积换不来那么长的打包时间
Compression=lzma2/normal
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "zh"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "快捷方式:"
Name: "autostart"; Description: "开机时自动运行（之后可在设置里改）"; \
  GroupDescription: "启动:"; Flags: unchecked

[Files]
Source: "{#Payload}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\词典"; Filename: "{app}\dict.exe"; WorkingDir: "{app}"
Name: "{group}\卸载词典"; Filename: "{uninstallexe}"
Name: "{autodesktop}\词典"; Filename: "{app}\dict.exe"; WorkingDir: "{app}"; \
  Tasks: desktopicon

[Registry]
; 勾了才写。值名和写法要和程序自己写的一致（见 autostart.rs），
; 否则设置页里的那个勾会和这里各说各话。
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; \
  ValueType: string; ValueName: "yinyidict"; ValueData: """{app}\dict.exe"""; \
  Flags: uninsdeletevalue; Tasks: autostart
; 没勾也要保证卸载时把它清掉 —— 用户可能是在程序设置里开的自启，
; 留一条指向已删除 exe 的启动项是很讨厌的。ValueType: none 表示安装时不写。
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; \
  ValueType: none; ValueName: "yinyidict"; Flags: uninsdeletevalue

[UninstallDelete]
; 运行期才生成的东西，Inno 不知道它们的存在，得点名删
Type: files; Name: "{app}\settings.json"
Type: dirifempty; Name: "{app}"

[Run]
Filename: "{app}\dict.exe"; Description: "现在就打开词典"; \
  WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent
