package dev.fanchao.cpxy.rtp

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.core.content.edit
import dev.fanchao.cpxy.rtp.engine.RtpException
import dev.fanchao.cpxy.rtp.engine.deriveKey
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Keeps each profile's 32-byte tunnel key encrypted with an AES key that never leaves the Android
 * Keystore. The password itself is not kept.
 */
class AndroidKeyVault(context: Context) : KeyVault {
    private val preferences = context.getSharedPreferences("tunnel_keys", Context.MODE_PRIVATE)

    override suspend fun store(profileId: String, password: String) = withContext(Dispatchers.Default) {
        val key = try {
            deriveKey(password)
        } catch (e: RtpException) {
            throw IllegalArgumentException((e as? RtpException.Failed)?.reason ?: "Invalid password", e)
        }
        try {
            val cipher = Cipher.getInstance(TRANSFORMATION).apply { init(Cipher.ENCRYPT_MODE, wrappingKey()) }
            val sealed = cipher.iv + cipher.doFinal(key)
            preferences.edit { putString(profileId, Base64.encodeToString(sealed, Base64.NO_WRAP)) }
        } finally { key.fill(0) }
    }

    override suspend fun contains(profileId: String): Boolean = preferences.contains(profileId)

    override suspend fun delete(profileId: String) {
        preferences.edit { remove(profileId) }
    }

    /** The profile's key, or null when it has none or it cannot be decrypted. */
    fun load(profileId: String): ByteArray? = runCatching {
        val sealed = preferences.getString(profileId, null)?.let { Base64.decode(it, Base64.NO_WRAP) }
            ?: return null
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, wrappingKey(), GCMParameterSpec(128, sealed, 0, IV_LENGTH))
        cipher.doFinal(sealed, IV_LENGTH, sealed.size - IV_LENGTH).also {
            require(it.size == 32) { it.fill(0); "Invalid saved key" }
        }
    }.getOrNull()

    private fun wrappingKey(): SecretKey {
        val store = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        (store.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE).run {
            init(
                KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .setKeySize(256)
                    .build(),
            )
            generateKey()
        }
    }

    private companion object {
        const val KEYSTORE = "AndroidKeyStore"
        const val ALIAS = "tunnel-keys"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val IV_LENGTH = 12
    }
}
