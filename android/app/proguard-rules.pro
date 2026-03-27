# UniFFI generated bindings - keep all SDK types
-keep class com.zremote.sdk.** { *; }

# Keep native method names
-keepclasseswithmembernames class * {
    native <methods>;
}
