# PeerBeam R8/ProGuard rules.
# Flutter's embedding + platform channels.
-keep class io.flutter.** { *; }
-keep class io.flutter.plugins.** { *; }
-dontwarn io.flutter.**
# Keep the app's platform-channel Kotlin (foreground service, intents).
-keep class com.peerbeam.** { *; }
# QR scanning: mobile_scanner + MLKit barcode. R8 optimize otherwise nulls the
# MLKit scanner instance (NPE in the plugin's native start path).
-keep class dev.steenbakker.mobile_scanner.** { *; }
-keep class com.google.mlkit.** { *; }
-keep class com.google.android.gms.internal.mlkit_vision_barcode.** { *; }
-keep class com.google.android.gms.vision.** { *; }
-dontwarn com.google.mlkit.**
