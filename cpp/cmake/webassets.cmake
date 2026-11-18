# 前端产物：web/dist。源码比 web.stamp 新时自动重建。
find_program(CDC_NPM_EXECUTABLE npm)
if(NOT CDC_NPM_EXECUTABLE)
    message(FATAL_ERROR "找不到 npm，请先安装 Node.js 20+ 并加入 PATH。")
endif()

if(NOT EXISTS "${CDC_WEB_DIST}/index.html")
    message(WARNING "web/dist 不存在，将在构建时执行 npm run build。")
endif()

file(GLOB_RECURSE CDC_WEB_SOURCES CONFIGURE_DEPENDS
    "${CDC_REPO_ROOT}/web/src/*"
)
list(APPEND CDC_WEB_SOURCES
    "${CDC_REPO_ROOT}/web/index.html"
    "${CDC_REPO_ROOT}/web/package.json"
    "${CDC_REPO_ROOT}/web/vite.config.ts"
)

set(CDC_WEB_STAMP "${CMAKE_BINARY_DIR}/web.stamp")

add_custom_command(
    OUTPUT "${CDC_WEB_STAMP}"
    COMMAND cmd.exe /c "${CDC_NPM_EXECUTABLE}" --prefix "${CDC_REPO_ROOT}/web" run build
    COMMAND ${CMAKE_COMMAND} -E touch "${CDC_WEB_STAMP}"
    DEPENDS ${CDC_WEB_SOURCES}
    WORKING_DIRECTORY "${CDC_REPO_ROOT}/web"
    COMMENT "构建前端 web/dist"
    VERBATIM
)

add_custom_target(cdc_web DEPENDS "${CDC_WEB_STAMP}")
