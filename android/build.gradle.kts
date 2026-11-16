// 根构建脚本：仅集中声明插件版本，实际配置在各模块自有脚本中
plugins {
    id("com.android.application") version "8.7.3" apply false
    id("org.jetbrains.kotlin.android") version "2.0.21" apply false
}
