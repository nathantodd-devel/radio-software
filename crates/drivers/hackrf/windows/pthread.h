/*
 * The handful of POSIX thread functions libhackrf uses, on Windows' own
 * threads, locks and condition variables: enough to compile the library
 * into the program with Microsoft's compiler, which has no <pthread.h>,
 * without bringing in a pthreads library for it.
 *
 * Only what vendor/hackrf's hackrf.c calls is here, and only as it calls
 * it: default attributes, and no cancellation or timed waits.
 */
#ifndef DMPS_HACKRF_PTHREAD_H
#define DMPS_HACKRF_PTHREAD_H

#include <windows.h>
#include <process.h>
#include <stdlib.h>

typedef CRITICAL_SECTION pthread_mutex_t;
typedef CONDITION_VARIABLE pthread_cond_t;

/* A started thread: its handle, and what it was asked to run. */
struct dmps_thread {
	HANDLE handle;
	void* (*start)(void*);
	void* argument;
	void* result;
};
typedef struct dmps_thread* pthread_t;

static inline int pthread_mutex_init(pthread_mutex_t* mutex, const void* attributes)
{
	(void) attributes;
	InitializeCriticalSection(mutex);
	return 0;
}

static inline int pthread_mutex_destroy(pthread_mutex_t* mutex)
{
	DeleteCriticalSection(mutex);
	return 0;
}

static inline int pthread_mutex_lock(pthread_mutex_t* mutex)
{
	EnterCriticalSection(mutex);
	return 0;
}

static inline int pthread_mutex_unlock(pthread_mutex_t* mutex)
{
	LeaveCriticalSection(mutex);
	return 0;
}

static inline int pthread_cond_init(pthread_cond_t* condition, const void* attributes)
{
	(void) attributes;
	InitializeConditionVariable(condition);
	return 0;
}

/* Windows' condition variables hold nothing that needs freeing. */
static inline int pthread_cond_destroy(pthread_cond_t* condition)
{
	(void) condition;
	return 0;
}

static inline int pthread_cond_wait(pthread_cond_t* condition, pthread_mutex_t* mutex)
{
	return SleepConditionVariableCS(condition, mutex, INFINITE) ? 0 : 1;
}

static inline int pthread_cond_broadcast(pthread_cond_t* condition)
{
	WakeAllConditionVariable(condition);
	return 0;
}

static unsigned __stdcall dmps_thread_main(void* context)
{
	struct dmps_thread* thread = (struct dmps_thread*) context;
	thread->result = thread->start(thread->argument);
	return 0;
}

static inline int pthread_create(
	pthread_t* thread,
	const void* attributes,
	void* (*start)(void*),
	void* argument)
{
	struct dmps_thread* created = (struct dmps_thread*) malloc(sizeof(*created));
	(void) attributes;
	if (created == NULL) {
		return 1;
	}
	created->start = start;
	created->argument = argument;
	created->result = NULL;
	created->handle =
		(HANDLE) _beginthreadex(NULL, 0, dmps_thread_main, created, 0, NULL);
	if (created->handle == NULL) {
		free(created);
		return 1;
	}
	*thread = created;
	return 0;
}

/* Wait for a thread to finish; it can be joined once. */
static inline int pthread_join(pthread_t thread, void** result)
{
	if (WaitForSingleObject(thread->handle, INFINITE) != WAIT_OBJECT_0) {
		return 1;
	}
	if (result != NULL) {
		*result = thread->result;
	}
	CloseHandle(thread->handle);
	free(thread);
	return 0;
}

#endif
