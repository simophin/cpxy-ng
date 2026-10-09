-dontobfuscate

# JNA and the UniFFI bindings use reflection on these.
-keep class com.sun.jna.** { *; }
-keep class * extends com.sun.jna.** { *; }
-keep class dev.fanchao.cpxy.vpn.engine.** { *; }

-dontwarn java.awt.**
