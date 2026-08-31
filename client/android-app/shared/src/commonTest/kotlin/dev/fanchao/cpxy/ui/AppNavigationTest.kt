package dev.fanchao.cpxy.ui

import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.serialization.NavBackStackSerializer
import androidx.savedstate.serialization.decodeFromSavedState
import androidx.savedstate.serialization.encodeToSavedState
import kotlinx.serialization.PolymorphicSerializer
import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertNull
import kotlin.test.assertTrue

class AppNavigationTest {
    private val json = Json {
        serializersModule = AppNavigationSavedStateConfiguration.serializersModule
    }

    @Test
    fun editProfileRouteRetainsNullablePayloads() {
        assertEquals("profile-42", EditProfileRoute("profile-42").profileId)
        assertNull(EditProfileRoute(null).profileId)
    }

    @Test
    fun routeHierarchyRoundTripsPolymorphically() {
        val serializer = PolymorphicSerializer(NavKey::class)
        val routes: List<AppRoute> = listOf(
            HomeRoute,
            EditProfileRoute("profile-42"),
            EditProfileRoute(null),
        )

        routes.forEach { route ->
            val encoded = json.encodeToString(serializer, route)
            val restored = json.decodeFromString(serializer, encoded)
            assertEquals(route, restored)
            assertIs<AppRoute>(restored)
        }
    }

    @Test
    fun navigationBackStackRestoresWithExplicitConfiguration() {
        val serializer = NavBackStackSerializer(PolymorphicSerializer(NavKey::class))
        val original = NavBackStack<NavKey>(HomeRoute, EditProfileRoute("profile-42"))

        val savedState = encodeToSavedState(
            serializer = serializer,
            value = original,
            configuration = AppNavigationSavedStateConfiguration,
        )
        val restored = decodeFromSavedState(
            deserializer = serializer,
            savedState = savedState,
            configuration = AppNavigationSavedStateConfiguration,
        )

        assertEquals(original.toList(), restored.toList())
    }

    @Test
    fun popUnlessRootNeverEmptiesTheStack() {
        val stack = mutableListOf<AppRoute>(HomeRoute)

        assertFalse(stack.popUnlessRoot())
        assertEquals(listOf<AppRoute>(HomeRoute), stack)

        stack += EditProfileRoute("profile-42")
        assertTrue(stack.popUnlessRoot())
        assertEquals(listOf<AppRoute>(HomeRoute), stack)
    }
}
