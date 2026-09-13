package com.localtrans.app.bridge

import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runTest
import uniffi.localtrans_ffi.AppEvent
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class EventRouterTest {

    @Test
    fun testEventRouterEmitsEvents() = runTest {
        // Create a test event
        val testEvent = AppEvent.Hello("test_event")

        // Collect events
        val collectedEvents = mutableListOf<AppEvent>()
        val job = launch {
            EventRouter.events.collect { event ->
                collectedEvents.add(event)
            }
        }
        // 订阅建立后再发射(yield 让 collect 先挂起;流带 replay=8,跨用例残留会先补发)
        kotlinx.coroutines.yield()

        // Route an event
        EventRouter.route(testEvent)

        // Give time for collection
        kotlinx.coroutines.delay(100)

        // Verify event was routed
        assertTrue(collectedEvents.isNotEmpty(), "Should have collected events")
        // events 流 replay=8(单例跨用例残留会补发)——只断言本用例发射的事件在尾部
        assertEquals(testEvent, collectedEvents.last())

        job.cancel()
    }

    @Test
    fun testMultipleEventTypes() = runTest {
        val events = listOf(
            AppEvent.Hello("event1"),
            AppEvent.Hello("event2"),
            AppEvent.Hello("event3")
        )

        val collectedEvents = mutableListOf<AppEvent>()
        val job = launch {
            EventRouter.events.collect { event ->
                collectedEvents.add(event)
            }
        }
        kotlinx.coroutines.yield()

        events.forEach { EventRouter.route(it) }
        kotlinx.coroutines.delay(100)

        // events 流 replay=8(单例跨用例残留会补发)——断言尾部即本用例的 3 个事件
        assertEquals(events, collectedEvents.takeLast(events.size), "Should collect all 3 events")

        job.cancel()
    }
}

