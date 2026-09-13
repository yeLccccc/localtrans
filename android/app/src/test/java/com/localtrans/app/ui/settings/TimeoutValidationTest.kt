package com.localtrans.app.ui.settings

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TimeoutValidationTest {

    @Test
    fun `normal value is valid`() {
        assertFalse(isTimeoutInvalid("60"))
    }

    @Test
    fun `empty string is invalid`() {
        assertTrue(isTimeoutInvalid(""))
    }

    @Test
    fun `non numeric is invalid`() {
        assertTrue(isTimeoutInvalid("abc"))
    }

    @Test
    fun `below minimum is invalid`() {
        assertTrue(isTimeoutInvalid("14"))
    }

    @Test
    fun `above maximum is invalid`() {
        assertTrue(isTimeoutInvalid("601"))
    }

    @Test
    fun `boundaries are valid`() {
        assertFalse(isTimeoutInvalid("15"))
        assertFalse(isTimeoutInvalid("600"))
    }
}
