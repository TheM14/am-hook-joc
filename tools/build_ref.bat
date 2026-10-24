@echo off
rem Builds the optional reference dump tool (Windows, MSVC).
rem
rem Set these before running, or pass them on the command line:
rem   VCVARS    path to vcvars64.bat of your Visual Studio installation
rem   JOC_REF   checkout of the C++ reference project (include/ directory)
rem   JOC_CORE  directory holding joc_core.lib
rem
rem Example:
rem   set JOC_REF=C:\path\to\JustOneCacophonyCXX
rem   set JOC_CORE=C:\path\to\joc-core
rem   set VCVARS=C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat
rem   tools\build_ref.bat

if "%VCVARS%"=="" (echo error: set VCVARS to vcvars64.bat & exit /b 1)
if "%JOC_REF%"=="" (echo error: set JOC_REF to the reference checkout & exit /b 1)
if "%JOC_CORE%"=="" (echo error: set JOC_CORE to the directory with joc_core.lib & exit /b 1)

call "%VCVARS%" >nul
cl /nologo /std:c++20 /O2 /EHsc /I "%JOC_REF%\include" ref_dump.cpp /link /LIBPATH:"%JOC_CORE%\lib" joc_core.lib /out:ref_dump.exe
