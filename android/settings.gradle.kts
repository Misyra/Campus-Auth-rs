// Android 壳工程设置：插件与依赖仓库声明 + 模块列表
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    // 依赖只允许从本块声明的仓库解析，禁止模块脚本私自加仓库
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "campus-auth-android"
include(":app")
