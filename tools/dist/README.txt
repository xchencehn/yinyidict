词典 · 离线中英词典
https://github.com/xchencehn/yinyidict

用法
----
1. 双击 setup.bat，等它把数据下好、索引建好（约 850 MB，几分钟）。
2. 之后双击 dict.exe。
3. 按 Alt+1 随时唤出，关窗口是收进托盘，真要退出走托盘菜单。

为什么要先跑一次
----------------
词库数据不能跟着这个压缩包一起分发（授权见下），所以只能装好程序之后
由它自己去各家官方地址下载。整个过程不需要你另外装任何东西 ——
用的是 Windows 自带的 curl 和 tar。

授权
----
程序代码：MIT。

词典数据不属于本项目，各有各的授权，由 setup.bat 从各自官方地址下载：

  CC-CEDICT   中文词条 / 拼音 / 释义      CC BY-SA 3.0
  ECDICT      英文词条                    授权不明确，自行判断
  Tatoeba     例句                        CC BY 2.0 FR
  Unihan      异体字                      Unicode License
  Kokoro      语音模型                    Apache 2.0
  sherpa-onnx 推理运行库                  Apache 2.0

拿它做再分发之前，上面这几行需要你自己核实一遍。

已知的事
--------
- 只支持 Windows x64。
- 头一次启动语音引擎要几秒（模型 311 MB），期间界面照常能查词。
- 有 NVIDIA 显卡且装了 CUDA + cuDNN 的话发音会快 9 倍，否则自动用 CPU。

（脚本叫 setup.bat 而不是中文名：压缩包里的中文文件名在不同解压工具下
会变成乱码，宁可名字朴素点，也不要一个双击不开的文件。）
