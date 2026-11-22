// 主模块：Kotlin 薄壳（前台服务常驻 Rust 认证核心 + WebView 加载本机 axum 服务）
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "rs.campusauth.app"
    compileSdk = 35

    defaultConfig {
        applicationId = "rs.campusauth.app"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0-alpha"
    }

    buildTypes {
        release {
            // P0 未配置签名：assembleRelease 产出未签名 APK，签名方案见 android/README.md
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }

    kotlinOptions {
        jvmTarget = "11"
    }

    sourceSets {
        // jniLibs 显式指向 src/main/jniLibs：cargo-ndk 产出的 libcampus_auth.so
        // 按 ABI 子目录（arm64-v8a 等）拷入后随 APK 打包
        getByName("main") {
            jniLibs.srcDir("src/main/jniLibs")
        }
    }
}

dependencies {
    // 唯一外部依赖：兼容性工具（NotificationCompat / ServiceCompat / ContextCompat / ViewCompat 等）
    // 不引入 appcompat / material，主题直接继承系统 Theme.Material.Light.NoActionBar
    implementation("androidx.core:core-ktx:1.15.0")
}
