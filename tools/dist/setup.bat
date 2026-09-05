@echo off
chcp 65001 >nul
cd /d "%~dp0"
echo.
echo   词典 · 首次运行
echo   ================
echo.
echo   接下来会下载词典数据、语音模型和运行库（约 850 MB），
echo   然后在本机建好索引。头一次要等几分钟，之后就不用再跑了。
echo.
echo   数据来自 CC-CEDICT / ECDICT / Tatoeba / Unicode 和 k2-fsa，
echo   各自的授权见 README.txt。
echo.
pause

dict-build.exe fetch --root .
if errorlevel 1 goto fail

echo.
echo   正在建索引（约半分钟）…
dict-build.exe --data data\raw --out data\index
if errorlevel 1 goto fail

echo.
echo   好了。以后直接双击 dict.exe 就行。
echo.
echo   原始数据（data\raw，约 400 MB）现在可以删掉，索引已经建好了。
pause
exit /b 0

:fail
echo.
echo   出错了。上面几行是原因；网络不通的话重跑一次即可，
echo   已经下好的部分会跳过。
pause
exit /b 1
