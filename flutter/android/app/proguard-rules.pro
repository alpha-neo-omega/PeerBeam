# PeerBeam R8/ProGuard rules.
# Flutter's embedding + platform channels.
-keep class io.flutter.** { *; }
-keep class io.flutter.plugins.** { *; }
-dontwarn io.flutter.**
# Keep the app's platform-channel Kotlin (foreground service, intents).
-keep class com.peerbeam.** { *; }
