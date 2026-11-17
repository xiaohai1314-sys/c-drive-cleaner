fn main() {
    // 前端产物由 Vite 生成，改动后必须重新嵌入，否则 exe 里还是旧界面。
    println!("cargo:rerun-if-changed=../web/dist");
    tauri_build::build()
}
