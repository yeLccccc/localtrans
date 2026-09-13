# Keep uniFFI generated classes
-keep class uniffi.localtrans.** { *; }
-keep class uniffi.localtrans.*.** { *; }

# Keep AppEvent enum
-keep class uniffi.localtrans.AppEvent { *; }
-keep class uniffi.localtrans.AppEvent$* { *; }

# Keep callback interface
-keep class com.localtrans.app.LocalTransBridge$Callback { *; }

# Keep Kotlin data classes
-keep class com.localtrans.app.bridge.** { *; }

# Keep Compose-generated classes
-keep class androidx.compose.** { *; }
-keep class kotlin.Metadata { *; }

# JNA — native 层按字段名/方法名反射查找,混淆即崩(UnsatisfiedLinkError: peer field ID)
-keep class com.sun.jna.** { *; }
-dontwarn com.sun.jna.**

# R8 字段名重命名(zero size Structure):uniFFI 生成的 JNA Structure 字段是
# native 布局的映射,名字改了 JNA deriveLayout 就算不出大小
-keep class uniffi.localtrans_ffi.RustBuffer* { *; }
-keep class uniffi.localtrans_ffi.ForeignBytes* { *; }
-keep class uniffi.localtrans_ffi.UniffiRustCallStatus* { *; }

# 回调 VTable 同为 JNA Structure;且 internal object 单例跨类引用被 R8 内联引发 ClassCastException。
# uniffi 包整体禁止混淆/优化(体积代价 ~1MB,换来可靠)
-keep class uniffi.localtrans_ffi.** { *; }
-keepnames class uniffi.localtrans_ffi.** { *; }
-dontoptimize
-dontobfuscate
