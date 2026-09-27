/* Diagnostic JNI shim. No moving Lisp values cross this interface. */
#define _GNU_SOURCE
#include <jni.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <stdint.h>

static JavaVM *vm;
static jclass workload;
static jmethodID methods[9];
static const char *names[] = {"twice", "recurse", "foreignThread", "collect",
                            "overflow", "nullFault", "throwing", "pause", "overlapping"};
static int (*lisp_callback)(int);
static int start_status = -1;
static struct sigaction jvm_segv;
static _Atomic int active_calls;

static void fatal(const char *message) {
    fprintf(stderr, "JVM-PROBE: %s\n", message);
    fflush(stderr);
    _Exit(90);
}

static int clear_exception(JNIEnv *env) {
    if (!(*env)->ExceptionCheck(env)) return 0;
    (*env)->ExceptionClear(env);
    return 1;
}

int probe_thread_context(void) {
    pthread_attr_t attr;
    void *base;
    size_t size;
    char local;
    stack_t alternate;
    if (pthread_getattr_np(pthread_self(), &attr)) fatal("thread attributes unavailable");
    if (pthread_attr_getstack(&attr, &base, &size)) fatal("thread stack unavailable");
    pthread_attr_destroy(&attr);
    if (sigaltstack(NULL, &alternate)) fatal("alternate stack unavailable");
    uintptr_t here = (uintptr_t)&local;
    return (here >= (uintptr_t)base && here < (uintptr_t)base + size ? 1 : 0)
           | ((alternate.ss_flags & SS_DISABLE) ? 0 : 2);
}

static jint callback(JNIEnv *env, jclass cls, jint value) {
    (void)env; (void)cls;
    if (!lisp_callback) fatal("callback not registered");
    static _Thread_local int reported;
    if (!reported) {
        fprintf(stderr, "CALLBACK-STACK-CONTEXT=%d (bit0=pthread-stack bit1=alt-stack)\n",
                probe_thread_context());
        reported = 1;
    }
    return lisp_callback(value);
}

static void *create_vm(void *unused) {
    (void)unused;
    const char *directory = getenv("TORCL_JVM_PROBE_DIR");
    if (!directory) fatal("TORCL_JVM_PROBE_DIR missing");
    char *classpath;
    if (asprintf(&classpath, "-Djava.class.path=%s", directory) < 0)
        fatal("classpath allocation failed");
    JavaVMOption options[] = {{classpath, NULL}, {"-Xmx128m", NULL},
                             {"-Xcheck:jni", NULL}, {"-Xrs", NULL}};
    JavaVMInitArgs args = {JNI_VERSION_1_8, 4, options, JNI_FALSE};
    JNIEnv *env;
    start_status = JNI_CreateJavaVM(&vm, (void **)&env, &args);
    free(classpath);
    if (start_status != JNI_OK) return NULL;
    jclass local = (*env)->FindClass(env, "Coexistence");
    if (clear_exception(env) || !local) fatal("class lookup failed");
    workload = (*env)->NewGlobalRef(env, local);
    if (clear_exception(env) || !workload) fatal("global reference failed");
    (*env)->DeleteLocalRef(env, local);
    JNINativeMethod native = {"callback", "(I)I", (void *)callback};
    if ((*env)->RegisterNatives(env, workload, &native, 1) != JNI_OK)
        fatal("native registration failed");
    for (size_t i = 0; i < sizeof(methods)/sizeof(*methods); ++i) {
        methods[i] = (*env)->GetStaticMethodID(env, workload, names[i], "(I)I");
        if (clear_exception(env) || !methods[i]) fatal("method lookup failed");
    }
    if (sigaction(SIGSEGV, NULL, &jvm_segv)) fatal("signal query failed");
    if ((*vm)->DetachCurrentThread(vm) != JNI_OK) fatal("creator detach failed");
    return NULL;
}

int probe_start(void) {
    if (vm) return start_status;
    pthread_t thread;
    if (pthread_create(&thread, NULL, create_vm, NULL)) fatal("creator thread failed");
    if (pthread_join(thread, NULL)) fatal("creator join failed");
    return start_status;
}

__attribute__((constructor)) static void preload_start(void) {
    const char *order = getenv("TORCL_JVM_PROBE_ORDER");
    if (order && !strcmp(order, "jvm-first") && probe_start())
        fatal("preloaded JVM creation failed");
}

static JNIEnv *enter(int *attached) {
    JNIEnv *env = NULL;
    if (!vm) fatal("JVM not initialized");
    jint status = (*vm)->GetEnv(vm, (void **)&env, JNI_VERSION_1_8);
    *attached = status == JNI_EDETACHED;
    if (*attached) status = (*vm)->AttachCurrentThreadAsDaemon(vm, (void **)&env, NULL);
    if (status != JNI_OK) fatal("thread attachment failed");
    return env;
}

static void leave(int attached) {
    if (attached && (*vm)->DetachCurrentThread(vm) != JNI_OK)
        fatal("thread detachment failed");
}

int probe_call(int which, int value) {
    if (which < 0 || which >= 9) fatal("bad method index");
    int attached;
    JNIEnv *env = enter(&attached);
    atomic_fetch_add(&active_calls, 1);
    jint result = (*env)->CallStaticIntMethod(env, workload, methods[which], value);
    int threw = clear_exception(env);
    atomic_fetch_sub(&active_calls, 1);
    leave(attached);
    return threw ? -200 : result;
}

void probe_set_callback(void *pointer) { lisp_callback = pointer; }
int probe_active(void) { return atomic_load(&active_calls); }
int probe_handler_preserved(void) {
    struct sigaction current;
    if (sigaction(SIGSEGV, NULL, &current)) fatal("signal query failed");
    return current.sa_sigaction == jvm_segv.sa_sigaction;
}

/* Exercise explicit global-reference lifetime, not a distributed collector. */
int probe_handles(int count) {
    int attached;
    JNIEnv *env = enter(&attached);
    for (int i = 0; i < count; ++i) {
        jstring local = (*env)->NewStringUTF(env, "handle-probe");
        if (clear_exception(env) || !local) fatal("string allocation failed");
        jobject global = (*env)->NewGlobalRef(env, local);
        if (clear_exception(env) || !global) fatal("global allocation failed");
        (*env)->DeleteLocalRef(env, local);
        if ((*env)->GetStringLength(env, (jstring)global) != 12)
            fatal("global reference contents changed");
        (*env)->DeleteGlobalRef(env, global);
    }
    leave(attached);
    return count;
}

/* Same attached native frame, cached ID; includes JNI call and exception check. */
double probe_cached_ns(int count) {
    if (count < 1) fatal("empty benchmark");
    int attached;
    JNIEnv *env = enter(&attached);
    struct timespec begin, end;
    for (int i = 0; i < 20000; ++i) {
        jint result = (*env)->CallStaticIntMethod(env, workload, methods[0], 21);
        if (clear_exception(env) || result != 42) fatal("warmup failed");
    }
    clock_gettime(CLOCK_MONOTONIC, &begin);
    for (int i = 0; i < count; ++i) {
        jint result = (*env)->CallStaticIntMethod(env, workload, methods[0], 21);
        if (clear_exception(env) || result != 42) fatal("benchmark failed");
    }
    clock_gettime(CLOCK_MONOTONIC, &end);
    leave(attached);
    return ((end.tv_sec - begin.tv_sec) * 1e9 + end.tv_nsec - begin.tv_nsec) / count;
}

static void *destroy_vm(void *unused) {
    (void)unused;
    int attached;
    JNIEnv *env = enter(&attached);
    if ((*env)->UnregisterNatives(env, workload) != JNI_OK) fatal("unregister failed");
    (*env)->DeleteGlobalRef(env, workload);
    workload = NULL;
    start_status = (*vm)->DestroyJavaVM(vm);
    if (start_status == JNI_OK) vm = NULL;
    /* DestroyJavaVM detaches its caller; no further JNI calls are legal. */
    return NULL;
}

int probe_stop(void) {
    if (atomic_load(&active_calls) || lisp_callback) fatal("shutdown with live bridge users");
    pthread_t thread;
    if (pthread_create(&thread, NULL, destroy_vm, NULL)) fatal("shutdown thread failed");
    if (pthread_join(thread, NULL)) fatal("shutdown join failed");
    return start_status;
}
