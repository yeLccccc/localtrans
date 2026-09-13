@echo off
rem 测试机启动脚本：Token 从同目录 test-api.key 读取（该文件不入库）
set LOCALTRANS_TEST_API=1
set /p LOCALTRANS_TEST_API_KEY=<"%~dp0test-api.key"
set LOCALTRANS_TEST_API_PORT=39871
cd /d "%~dp0"
start "" localtrans.exe
