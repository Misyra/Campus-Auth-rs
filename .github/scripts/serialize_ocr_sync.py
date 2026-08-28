from pathlib import Path

path = Path("src/environment/mod.rs")
source = path.read_text(encoding="utf-8")

replacements = [
    (
        '''    /// 安装 OCR 依赖（`uv add ddddocr`）。
    async fn install_ocr_dep(&self) -> Result<(), EnvironmentError>;
    /// 卸载 OCR 依赖（`uv remove ddddocr`）。
    async fn remove_ocr_dep(&self) -> Result<(), EnvironmentError>;
    /// OCR 依赖（ddddocr）是否已安装在 venv 内。
    fn ocr_ready(&self) -> bool;
    /// 项目是否在 `python_worker/pyproject.toml` 中声明了 ddddocr 依赖。
    fn ocr_declared(&self) -> bool;
''',
        '''    /// 安装 OCR optional extra，并持久记录用户启用偏好。
    async fn install_ocr_dep(&self) -> Result<(), EnvironmentError>;
    /// 卸载 OCR optional extra，并清除用户启用偏好。
    async fn remove_ocr_dep(&self) -> Result<(), EnvironmentError>;
    /// OCR 依赖（ddddocr）是否已安装在 venv 内。
    fn ocr_ready(&self) -> bool;
    /// 项目是否声明支持 `ocr` optional extra。
    fn ocr_declared(&self) -> bool;
''',
        "trait docs",
    ),
    (
        '''    async fn install_ocr_dep(&self) -> Result<(), EnvironmentError> {
        crate::environment::uv::install_ocr_dep(self).await
    }

    async fn remove_ocr_dep(&self) -> Result<(), EnvironmentError> {
        crate::environment::uv::remove_ocr_dep(self).await
    }
''',
        '''    async fn install_ocr_dep(&self) -> Result<(), EnvironmentError> {
        self.bootstrap_gate
            .run_exclusive(crate::environment::uv::install_ocr_dep(self))
            .await
    }

    async fn remove_ocr_dep(&self) -> Result<(), EnvironmentError> {
        self.bootstrap_gate
            .run_exclusive(crate::environment::uv::remove_ocr_dep(self))
            .await
    }
''',
        "serialize ocr sync",
    ),
]

for old, new, label in replacements:
    count = source.count(old)
    if count != 1:
        raise SystemExit(f"{label}: 预期 1 个锚点，实际 {count}")
    source = source.replace(old, new, 1)
    print(f"{label}: ok")

path.write_text(source, encoding="utf-8")
