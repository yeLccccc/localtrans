plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

import java.io.File
import java.io.FileInputStream
import java.util.Properties

val keystoreProps = Properties().apply {
    val f = rootProject.file("keystore.properties")
    if (f.exists()) load(FileInputStream(f))
}

android {
    namespace = "com.localtrans.app"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.localtrans.app"
        minSdk = 26
        targetSdk = 35
        versionCode = 17
        versionName = "0.13.0"
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    signingConfigs {
        create("release") {
            if (keystoreProps.isNotEmpty()) {
                storeFile = rootProject.file(keystoreProps.getProperty("storeFile"))
                storePassword = keystoreProps.getProperty("storePassword")
                keyAlias = keystoreProps.getProperty("keyAlias")
                keyPassword = keystoreProps.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            signingConfig = signingConfigs.getByName("release")
        }
    }

    // M7:BuildConfig.DEBUG 供 debug 测试钩子门控(EventRouter 自动同意配对、
    // 设置页测试钩子区块)。AGP 8 默认关闭生成,此处显式开启。
    buildFeatures {
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation(platform("androidx.compose:compose-bom:2024.10.01"))
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.navigation:navigation-compose:2.8.4")
    implementation("androidx.lifecycle:lifecycle-viewmodel-compose:2.8.7")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    implementation("io.coil-kt:coil-compose:2.7.0")
    implementation("io.coil-kt:coil-video:2.7.0")
    implementation("net.java.dev.jna:jna:5.14.0@aar")
    testImplementation("junit:junit:4.13.2")
    testImplementation("app.cash.turbine:turbine:1.2.0")
    testImplementation("org.jetbrains.kotlin:kotlin-test")
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.9.0")
}

// Rust 构建集成
val rustRoot = rootProject.projectDir.parentFile

// M7:NDK 路径配置化(spec §7.3"换机不断")——优先级:
// 1) android/local.properties 的 ndk.dir(显式指定)
// 2) sdk.dir 下 ndk/ 目录取版本号字典序最高的一个(标准 SDK 布局)
// 3) 环境变量 ANDROID_NDK_HOME(本机私有路径只存 android/local.properties,不入库)
val localProps = Properties().apply {
    val f = rootProject.file("local.properties")
    if (f.exists()) load(FileInputStream(f))
}
val ndkHome: String = run {
    val explicit = localProps.getProperty("ndk.dir")
    if (explicit != null) return@run explicit
    val sdkDir = localProps.getProperty("sdk.dir")
    if (sdkDir != null) {
        val ndkRoot = File(sdkDir, "ndk")
        val best = ndkRoot.listFiles()
            ?.filter { dir -> dir.isDirectory }
            ?.maxByOrNull { dir -> dir.name }
        if (best != null) return@run best.absolutePath
    }
    System.getenv("ANDROID_NDK_HOME")
    ?: throw GradleException("未找到 Android NDK:请在 android/local.properties 配 ndk.dir,或设置 ANDROID_NDK_HOME 环境变量")
}
// cargo.bin 为 Git Bash 形式的 cargo 可执行目录;本机私有路径只存 android/local.properties(已 gitignore)
val cargoBinPath: String = localProps.getProperty("cargo.bin")
    ?: throw GradleException("android/local.properties 缺少 cargo.bin 配置(示例: /c/<user>/.cargo/bin)")

tasks.register<Exec>("buildRustSo") {
    workingDir(rustRoot)
    commandLine("bash", "-c",
        """
        export ANDROID_NDK_HOME="$ndkHome"
        export CARGO_HOME="$cargoBinPath"
        export PATH="$cargoBinPath:$${"PATH"}"
        cargo ndk -t arm64-v8a -t x86_64 -o android/app/src/main/jniLibs build -p localtrans-ffi --release
        """.trimIndent()
    )
}

tasks.register<Exec>("genUniffi") {
    dependsOn("buildRustSo")
    workingDir(rustRoot)
    // Windows 下 uniffi-bindgen 对相对 --out-dir 静默不写文件(exit 仍 0);
    // 生成到绝对路径临时目录再拷回,见 docs/superpowers/specs/2026-08-26-android-copyip-manual-add-design.md
    commandLine("bash", "-c",
        """
        export ANDROID_NDK_HOME="$ndkHome"
        export CARGO_HOME="$cargoBinPath"
        export PATH="$cargoBinPath:$${"PATH"}"
        rm -rf target/uniffi-out && mkdir -p target/uniffi-out
        cargo run -p localtrans-ffi --bin uniffi-bindgen -- generate \
            --library target/android/x86_64/liblocaltrans_ffi.so \
            --language kotlin \
            --out-dir target/uniffi-out/uniffi
        cp -r target/uniffi-out/uniffi/. android/app/src/main/java/uniffi/
        """.trimIndent()
    )
}

// Disable automatic Rust build for now - manual integration required
// tasks.named("preBuild") {
//     dependsOn("genUniffi")
// }
