# UniFFI generated bindings - keep all SDK types
-keep class com.zremote.sdk.** { *; }

# Keep native method names
-keepclasseswithmembernames class * {
    native <methods>;
}

# Kotlin serialization
-keepattributes *Annotation*, InnerClasses
-dontnote kotlinx.serialization.**
-keepclassmembers class kotlinx.serialization.json.** { *** Companion; }
-keepclasseswithmembers class kotlinx.serialization.json.** {
    kotlinx.serialization.KSerializer serializer(...);
}
-keep,includedescriptorclasses class com.zremote.**$$serializer { *; }
-keepclassmembers class com.zremote.** {
    *** Companion;
}

# Hilt
-keep class dagger.hilt.** { *; }
-keep class javax.inject.** { *; }
-keep class * extends dagger.hilt.android.internal.managers.ViewComponentManager$FragmentContextWrapper { *; }

# Compose - keep runtime metadata
-keep class androidx.compose.runtime.** { *; }
