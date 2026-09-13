@echo off
rem T1:端口跟随 LOCALTRANS_TEST_PORT_BASE(未设则默认 47600-47601,与 core ports 模块语义一致)
set "LT_FW_BASE=47600"
if defined LOCALTRANS_TEST_PORT_BASE set "LT_FW_BASE=%LOCALTRANS_TEST_PORT_BASE%"
set /a LT_FW_END=%LT_FW_BASE%+1
netsh advfirewall firewall add rule name="LocalTrans UDP In" dir=in action=allow protocol=UDP localport=%LT_FW_BASE%-%LT_FW_END% profile=any
netsh advfirewall firewall add rule name="LocalTrans UDP Out" dir=out action=allow protocol=UDP localport=%LT_FW_BASE%-%LT_FW_END% profile=any
