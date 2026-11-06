#!/bin/sh
# Registers EffectCraft's project type (.ecproj, and the XML copy .ecprojx) and makes EffectCraft
# its preferred application, unless the user chose another.
signature=application/x-vnd.storytold-EffectCraft
app=$(findpaths -p "$0" B_FIND_PATH_APPS_DIRECTORY EffectCraft 2>/dev/null)
[ -n "$app" ] && [ -e "$app" ] && mimeset -a -f "$app"
setmime -set application/x-effectcraft -short "EffectCraft project" -long "EffectCraft project" -extension ecproj -extension ecprojx
setmime -dump application/x-effectcraft 2>/dev/null | grep -q -- -preferredAppSig \
    || setmime -set application/x-effectcraft -preferredAppSig "$signature"
exit 0
