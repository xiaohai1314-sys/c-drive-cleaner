# 把文本文件嵌入为 C++ 字符串常量。
# MSVC 单个字符串字面量约 16KB 上限，这里按块拼接。
# 用法: cmake -DINPUT=... -DOUTPUT=... -DVAR=kName -P embed_file.cmake
if(NOT DEFINED VAR)
    set(VAR kEmbedded)
endif()

file(READ "${INPUT}" CONTENT)
string(LENGTH "${CONTENT}" total)
set(chunk_size 12000)
set(index 0)
set(output "#pragma once\nstatic const char ${VAR}[] =\n")

while(index LESS total)
    math(EXPR remaining "${total} - ${index}")
    if(remaining GREATER chunk_size)
        set(length ${chunk_size})
    else()
        set(length ${remaining})
    endif()
    string(SUBSTRING "${CONTENT}" ${index} ${length} chunk)
    string(APPEND output "    R\"CDCSHIM(${chunk})CDCSHIM\"\n")
    math(EXPR index "${index} + ${length}")
endwhile()

string(APPEND output ";\n")
file(WRITE "${OUTPUT}" "${output}")
