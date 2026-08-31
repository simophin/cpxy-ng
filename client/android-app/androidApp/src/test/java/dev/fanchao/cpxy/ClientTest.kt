package dev.fanchao.cpxy

import com.sun.jna.Pointer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class ClientTest {
    @Test
    fun `rejects an incompatible ABI before creating a client`() {
        val client = FakeClient(abiVersion = 2)

        val error = assertThrows(IllegalStateException::class.java) {
            client.create(8080u, 1080u, 3010u, "1.1.1.1", "https://example.com", null, null)
        }

        assertTrue(error.message.orEmpty().contains("expected 1"))
        assertEquals(0, client.createCalls)
    }

    @Test
    fun `decodes a terminated native error`() {
        val client = FakeClient(createError = "invalid configuration")

        val error = assertThrows(RuntimeException::class.java) {
            client.create(8080u, 1080u, 3010u, "1.1.1.1", "not a url", null, null)
        }

        assertEquals("invalid configuration", error.message)
    }

    @Test
    fun `session close destroys its opaque handle exactly once`() {
        val client = FakeClient()
        val session = client.create(
            8080u,
            1080u,
            3010u,
            "1.1.1.1",
            "https://example.com",
            null,
            null,
        )

        session.close()
        session.close()

        assertEquals(1, client.destroyCalls)
    }

    private class FakeClient(
        private val abiVersion: Int = 1,
        private val createError: String? = null,
    ) : Client {
        var createCalls = 0
        var destroyCalls = 0

        override fun cpxy_client_abi_version(): Int = abiVersion

        override fun cpxy_client_create(
            httpProxyPort: Short,
            socks5ProxyPort: Short,
            apiServerPort: Short,
            dnsServer: String,
            mainServerUrl: String,
            aiServerUrl: String?,
            tailscaleServerUrl: String?,
            errorMessage: ByteArray,
            errorMessageCapacity: Int,
        ): Pointer? {
            createCalls += 1
            createError?.encodeToByteArray()?.let { bytes ->
                val copyLength = minOf(bytes.size, errorMessageCapacity - 1)
                bytes.copyInto(errorMessage, endIndex = copyLength)
                errorMessage[copyLength] = 0
                return null
            }
            return Pointer(0xCAFE)
        }

        override fun cpxy_client_destroy(instance: Pointer?) {
            destroyCalls += 1
        }
    }
}
