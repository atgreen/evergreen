#define _GNU_SOURCE
#include <jni.h>
#include <pthread.h>
#include <dlfcn.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>
#include <stdio.h>
#include <time.h>
#include "bridge_class.h"

typedef int64_t Handle;
typedef struct Reference {
    Handle id;
    jobject object;
    int weak, readers, closed;
    struct Reference *next;
} Reference;
typedef struct Callback {
    Handle id;
    Handle (*function)(Handle, Handle);
    unsigned active;
    struct Callback *next;
} Callback;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static JavaVM *vm;
static void *jvm_library;
static jclass helper, ambiguity_type;
static jmethodID dispatch_id, box_id, kind_id, integer_id, real_id, take_output_id;
static Reference *references;
static Callback *callbacks;
static Handle next_id = 1;
static unsigned active;
static int state; /* 0=fresh, 1=starting, 2=running, 3=stopping, 4=stopped/failed */
static int owns_vm;
static _Thread_local jchar error_text[4096];
static _Thread_local int error_length, error_kind;
static _Thread_local unsigned callback_depth;

int tj_error_kind(void) { return error_kind; }
int tj_error_length(void) { return error_length; }
void tj_error_copy(jchar *out) { if (error_length) memcpy(out, error_text, (size_t)error_length * sizeof(jchar)); }
static void clear_error(void) { error_kind = error_length = 0; }
static void fail(const char *text) {
    error_kind = 1;
    error_length = 0;
    while (*text && error_length < 4095) error_text[error_length++] = (unsigned char)*text++;
}
static int exception(JNIEnv *env) {
    if (!(*env)->ExceptionCheck(env)) return 0;
    jthrowable cause = (*env)->ExceptionOccurred(env);
    (*env)->ExceptionClear(env);
    fail("Java exception (diagnostic unavailable)");
    error_kind = ambiguity_type && (*env)->IsInstanceOf(env, cause, ambiguity_type) ? 3 : 2;
    jclass cls = (*env)->GetObjectClass(env, cause);
    jmethodID to_string = cls ? (*env)->GetMethodID(env, cls, "toString", "()Ljava/lang/String;") : NULL;
    if ((*env)->ExceptionCheck(env)) { (*env)->ExceptionClear(env); return 1; }
    jstring text = to_string ? (*env)->CallObjectMethod(env, cause, to_string) : NULL;
    if ((*env)->ExceptionCheck(env)) { (*env)->ExceptionClear(env); return 1; }
    if (text) {
        jsize length = (*env)->GetStringLength(env, text);
        error_length = length > 4095 ? 4095 : length;
        (*env)->GetStringRegion(env, text, 0, error_length, error_text);
        if (error_length && error_text[error_length-1] >= 0xd800 && error_text[error_length-1] <= 0xdbff) --error_length;
    }
    return 1;
}

/* An alternate/suspended fiber stack is not a supported HotSpot entry stack. */
static int native_stack(void) {
    pthread_attr_t attributes;
    void *base;
    size_t size;
    char marker;
    if (pthread_getattr_np(pthread_self(), &attributes)) { fail("Cannot inspect native thread stack"); return 0; }
    int rc = pthread_attr_getstack(&attributes, &base, &size);
    pthread_attr_destroy(&attributes);
    uintptr_t here = (uintptr_t)&marker;
    if (rc || here < (uintptr_t)base || here >= (uintptr_t)base + size) {
        fail("JVM calls require an ordinary native thread stack; switched fiber entry is unsupported");
        return 0;
    }
    return 1;
}
typedef struct Entry { JNIEnv *env; int attached, counted; } Entry;
static void leave(Entry *entry) {
    if (entry->env) {
        exception(entry->env);
        (*entry->env)->PopLocalFrame(entry->env, NULL);
        if (entry->attached) (*vm)->DetachCurrentThread(vm);
    }
    if (entry->counted) { pthread_mutex_lock(&lock); --active; pthread_mutex_unlock(&lock); }
}
static int enter(Entry *entry) {
    memset(entry, 0, sizeof(*entry));
    clear_error();
    if (!native_stack()) return 0;
    pthread_mutex_lock(&lock);
    if (state != 2) { pthread_mutex_unlock(&lock); fail("JVM session is not running"); return 0; }
    ++active;
    entry->counted = 1;
    pthread_mutex_unlock(&lock);
    JNIEnv *env = NULL;
    jint rc = (*vm)->GetEnv(vm, (void **)&env, JNI_VERSION_1_8);
    if (rc == JNI_EDETACHED) {
        rc = (*vm)->AttachCurrentThreadAsDaemon(vm, (void **)&env, NULL);
        entry->attached = rc == JNI_OK;
    }
    if (rc != JNI_OK) { fail("Cannot attach current thread to JVM"); leave(entry); return 0; }
    if ((*env)->PushLocalFrame(env, 64) != JNI_OK) {
        exception(env);
        if (entry->attached) (*vm)->DetachCurrentThread(vm);
        leave(entry);
        return 0;
    }
    entry->env = env;
    return 1;
}

static void delete_reference(JNIEnv *env, Reference *reference) {
    if (reference->weak) (*env)->DeleteWeakGlobalRef(env, reference->object);
    else (*env)->DeleteGlobalRef(env, reference->object);
    free(reference);
}
/* Called with lock held. The returned entry is now detached from the table. */
static Reference *unlink_reference(Reference *reference) {
    Reference **link = &references;
    while (*link && *link != reference) link = &(*link)->next;
    if (*link) *link = reference->next;
    return reference;
}
static Handle retain(JNIEnv *env, jobject object, int weak) {
    Reference *reference = calloc(1, sizeof(*reference));
    if (!reference) { fail("Native reference allocation failed"); return 0; }
    reference->object = weak ? (*env)->NewWeakGlobalRef(env, object) : (*env)->NewGlobalRef(env, object);
    reference->weak = weak;
    if (exception(env)) { free(reference); return 0; }
    pthread_mutex_lock(&lock);
    if (next_id >= (INT64_C(1) << 60)) {
        pthread_mutex_unlock(&lock); delete_reference(env, reference); fail("Handle identifiers exhausted"); return 0;
    }
    reference->id = next_id++;
    reference->next = references;
    references = reference;
    Handle id = reference->id;
    pthread_mutex_unlock(&lock);
    return id;
}
static jobject local(JNIEnv *env, Handle id, int allow_cleared) {
    if (!id) return NULL;
    pthread_mutex_lock(&lock);
    Reference *reference = references;
    while (reference && reference->id != id) reference = reference->next;
    if (!reference || reference->closed) { pthread_mutex_unlock(&lock); fail("Invalid or released Java reference"); return NULL; }
    ++reference->readers;
    pthread_mutex_unlock(&lock);
    jobject result = (*env)->NewLocalRef(env, reference->object);
    if (exception(env)) result = NULL;
    if (!result && reference->weak && !allow_cleared && !error_kind) fail("Weak Java reference was collected");
    pthread_mutex_lock(&lock);
    --reference->readers;
    Reference *retired = reference->closed && !reference->readers ? unlink_reference(reference) : NULL;
    pthread_mutex_unlock(&lock);
    if (retired) delete_reference(env, retired);
    return result;
}
static int release(JNIEnv *env, Handle id) {
    pthread_mutex_lock(&lock);
    Reference *reference = references;
    while (reference && reference->id != id) reference = reference->next;
    if (!reference || reference->closed) { pthread_mutex_unlock(&lock); return 0; }
    reference->closed = 1;
    Reference *retired = !reference->readers ? unlink_reference(reference) : NULL;
    pthread_mutex_unlock(&lock);
    if (retired) delete_reference(env, retired);
    return 1;
}
int tj_release(Handle id) { Entry entry; if (!enter(&entry)) return 0; int result = release(entry.env, id); leave(&entry); return result; }
Handle tj_copy(Handle id, int weak, int promote) {
    Entry entry; if (!enter(&entry)) return 0;
    jobject object = local(entry.env, id, promote);
    Handle result = error_kind || (promote && !object) ? 0 : retain(entry.env, object, weak);
    leave(&entry); return result;
}
int tj_same(Handle a, Handle b) {
    Entry entry; if (!enter(&entry)) return 0;
    jobject first = local(entry.env, a, 0), second = local(entry.env, b, 0);
    int result = !error_kind && (*entry.env)->IsSameObject(entry.env, first, second);
    leave(&entry); return result;
}
Handle tj_box(int kind, int64_t integer, double real, const jchar *text, int length) {
    Entry entry; if (!enter(&entry)) return 0;
    JNIEnv *env = entry.env;
    jstring string = NULL;
    if (kind == 3) {
        if (length < 0 || (length && !text)) fail("Invalid string buffer");
        else string = (*env)->NewString(env, text, length);
    }
    Handle result = 0;
    if (!exception(env) && !error_kind) {
        jobject object = (*env)->CallStaticObjectMethod(env, helper, box_id, (jint)kind, (jlong)integer, (jdouble)real, string);
        if (!exception(env)) result = retain(env, object, 0);
    }
    leave(&entry); return result;
}
Handle tj_call(int op, Handle target, Handle name, Handle signature, const Handle *arguments, int count) {
    Entry entry; if (!enter(&entry)) return 0;
    JNIEnv *env = entry.env;
    Handle result = 0;
    if (op < 0 || op > 23 || count < 0 || count > 1024 || (count && !arguments)) fail("Invalid call arguments");
    else if ((*env)->EnsureLocalCapacity(env, count + 32) != JNI_OK) exception(env);
    if (!error_kind) {
        jobject receiver = local(env, target, 0);
        jobject method = local(env, name, 0);
        jobject descriptor = local(env, signature, 0);
        jclass object_class = (*env)->FindClass(env, "java/lang/Object");
        jobjectArray array = NULL;
        if (!exception(env) && object_class) array = (*env)->NewObjectArray(env, count, object_class, NULL);
        exception(env);
        for (int i = 0; array && !error_kind && i < count; ++i) {
            jobject argument = local(env, arguments[i], 0);
            if (!error_kind) (*env)->SetObjectArrayElement(env, array, i, argument);
            exception(env);
        }
        if (!error_kind) {
            jobject value = (*env)->CallStaticObjectMethod(env, helper, dispatch_id, (jint)op, receiver, method, descriptor, array);
            if (!exception(env)) result = retain(env, value, 0);
        }
    }
    leave(&entry); return result;
}
int tj_kind(Handle id) {
    Entry entry; if (!enter(&entry)) return -1;
    jobject value = local(entry.env, id, 0);
    int result = error_kind ? -1 : (*entry.env)->CallStaticIntMethod(entry.env, helper, kind_id, value);
    leave(&entry); return result;
}
int64_t tj_integer(Handle id) {
    Entry entry; if (!enter(&entry)) return 0;
    jobject value = local(entry.env, id, 0);
    int64_t result = error_kind ? 0 : (*entry.env)->CallStaticLongMethod(entry.env, helper, integer_id, value);
    leave(&entry); return result;
}
double tj_real(Handle id) {
    Entry entry; if (!enter(&entry)) return 0;
    jobject value = local(entry.env, id, 0);
    double result = error_kind ? 0 : (*entry.env)->CallStaticDoubleMethod(entry.env, helper, real_id, value);
    leave(&entry); return result;
}
int tj_text(Handle id, jchar *out, int capacity) {
    Entry entry; if (!enter(&entry)) return -1;
    JNIEnv *env = entry.env;
    jobject value = local(env, id, 0);
    int length = -1;
    jclass cls = (*env)->FindClass(env, "java/lang/String");
    if (!exception(env) && !error_kind) {
        if (!value || !(*env)->IsInstanceOf(env, value, cls)) fail("Expected a Java string");
        else {
            length = (*env)->GetStringLength(env, value);
            if (out) {
                if (capacity < length) fail("String buffer too small");
                else (*env)->GetStringRegion(env, value, 0, length, out);
            }
        }
    }
    leave(&entry); return length;
}

void tj_callback_error(const jchar *text, int length) {
    if (!callback_depth) { fail("No active Java callback"); return; }
    error_kind = 1;
    error_length = length < 0 ? 0 : length > 4095 ? 4095 : length;
    if (error_length) memcpy(error_text, text, (size_t)error_length * sizeof(jchar));
}
static void throw_error(JNIEnv *env) {
    jstring message = (*env)->NewString(env, error_text, error_length);
    if ((*env)->ExceptionCheck(env)) return;
    jclass type = (*env)->FindClass(env, "java/lang/IllegalStateException");
    if ((*env)->ExceptionCheck(env)) return;
    jmethodID constructor = (*env)->GetMethodID(env, type, "<init>", "(Ljava/lang/String;)V");
    if ((*env)->ExceptionCheck(env)) return;
    jobject error = (*env)->NewObject(env, type, constructor, message);
    if (!(*env)->ExceptionCheck(env) && error) (*env)->Throw(env, error);
}
static jobject JNICALL invoke_lisp(JNIEnv *env, jclass type, jlong id, jstring name, jobjectArray args) {
    (void)type;
    clear_error();
    if (!native_stack()) { throw_error(env); return NULL; }
    pthread_mutex_lock(&lock);
    Callback *callback = callbacks;
    while (callback && callback->id != id) callback = callback->next;
    if (state != 2 || !callback) {
        pthread_mutex_unlock(&lock); fail("Lisp callback has been released"); throw_error(env); return NULL;
    }
    ++callback->active; ++active;
    pthread_mutex_unlock(&lock);
    Handle method = retain(env, name, 0), arguments = retain(env, args, 0), result = 0;
    ++callback_depth;
    if (!error_kind) result = callback->function(method, arguments);
    --callback_depth;
    jobject answer = NULL;
    if (!result && !error_kind) fail("Lisp callback failed without a result");
    if (!error_kind) answer = local(env, result, 0);
    if (result) release(env, result); // callback transfers ownership of its return handle
    release(env, method); release(env, arguments);
    // Exception construction still enters the JVM: keep shutdown/revocation
    // excluded until the last JNI operation is complete.
    if (error_kind) { throw_error(env); answer = NULL; }
    pthread_mutex_lock(&lock);
    --callback->active; --active;
    pthread_mutex_unlock(&lock);
    return answer;
}
Handle tj_callback(void *function) {
    Entry entry; if (!enter(&entry)) return 0;
    Callback *callback = calloc(1, sizeof(*callback));
    Handle result = 0;
    if (!callback || !function) { free(callback); fail("Invalid callback allocation"); }
    else {
        callback->function = (Handle (*)(Handle, Handle))function;
        pthread_mutex_lock(&lock);
        if (next_id >= (INT64_C(1) << 60)) { pthread_mutex_unlock(&lock); free(callback); fail("Handle identifiers exhausted"); }
        else {
            callback->id = next_id++; callback->next = callbacks; callbacks = callback;
            result = callback->id; pthread_mutex_unlock(&lock);
        }
    }
    leave(&entry); return result;
}
int tj_callback_release(Handle id) {
    Entry entry; if (!enter(&entry)) return 0;
    pthread_mutex_lock(&lock);
    Callback **link = &callbacks;
    while (*link && (*link)->id != id) link = &(*link)->next;
    Callback *callback = *link;
    if (callback && callback->active) { pthread_mutex_unlock(&lock); fail("Cannot release an active callback"); leave(&entry); return 0; }
    if (callback) *link = callback->next;
    pthread_mutex_unlock(&lock);
    free(callback);
    leave(&entry); return 1;
}

typedef struct Start {
    const char *library, *classpath, *options;
    int attach;
    int failure, length;
    jchar message[4096];
} Start;
static void *start_worker(void *argument) {
    Start *start = argument;
    int creation_attempted = 0;
    clear_error();
    jvm_library = dlopen(start->library, RTLD_NOW | RTLD_GLOBAL);
    if (!jvm_library) { fail(dlerror()); goto done; }
    jint (*query)(JavaVM **, jsize, jsize *) = dlsym(jvm_library, "JNI_GetCreatedJavaVMs");
    jint (*create)(JavaVM **, void **, void *) = dlsym(jvm_library, "JNI_CreateJavaVM");
    if (!query || !create) { fail("Library does not implement JNI Invocation API"); goto done; }
    jsize count = 0;
    if (query(&vm, 1, &count) != JNI_OK) { fail("Cannot query existing JVM"); goto done; }
    JNIEnv *env = NULL;
    if (count) {
        if (!start->attach) { fail("A JVM already exists; explicitly request :attach t"); vm = NULL; goto done; }
        if ((*vm)->AttachCurrentThreadAsDaemon(vm, (void **)&env, NULL) != JNI_OK) { fail("Cannot attach JVM initialization thread"); goto done; }
    } else {
        if (start->attach) { fail("No existing JVM to attach"); goto done; }
        char *classpath = NULL, *options = strdup(start->options);
        if (asprintf(&classpath, "-Djava.class.path=%s", start->classpath) < 0 || !options) {
            free(classpath); free(options); fail("JVM option allocation failed"); goto done;
        }
        JavaVMOption opts[130] = {{"-Xrs", NULL}, {classpath, NULL}};
        int n = 2;
        char *cursor = NULL;
        for (char *option = strtok_r(options, "\n", &cursor); option; option = strtok_r(NULL, "\n", &cursor)) {
            if (n == 130) { fail("Too many JVM options"); break; }
            opts[n++].optionString = option;
        }
        JavaVMInitArgs args = {JNI_VERSION_1_8, n, opts, JNI_FALSE};
        jint rc = JNI_ERR;
        if (!error_kind) {
            creation_attempted = 1;
            rc = create(&vm, (void **)&env, &args);
        }
        free(classpath); free(options);
        if (rc != JNI_OK) { if (!error_kind) fail("JNI_CreateJavaVM failed"); goto done; }
        owns_vm = 1;
    }
    if ((*env)->PushLocalFrame(env, 32) != JNI_OK) { exception(env); (*vm)->DetachCurrentThread(vm); goto done; }
    jclass ambiguity_local = (*env)->DefineClass(env, "org/torcl/jvm/AmbiguousCall", NULL,
        (const jbyte *)ambiguouscall_class, (jsize)sizeof(ambiguouscall_class));
    if (!exception(env) && ambiguity_local) ambiguity_type = (*env)->NewGlobalRef(env, ambiguity_local);
    jclass api_local = exception(env) || error_kind ? NULL : (*env)->DefineClass(env, "org/torcl/jvm/Api", NULL,
        (const jbyte *)api_class, (jsize)sizeof(api_class));
    if (api_local) (*env)->DeleteLocalRef(env, api_local);
    jclass local_class = exception(env) || error_kind ? NULL : (*env)->DefineClass(env, "org/torcl/jvm/Bridge", NULL,
        (const jbyte *)bridge_class, (jsize)sizeof(bridge_class));
    if (!exception(env) && local_class) {
        helper = (*env)->NewGlobalRef(env, local_class);
        if (!exception(env) && helper) {
            JNINativeMethod method = {"invokeLisp", "(JLjava/lang/String;[Ljava/lang/Object;)Ljava/lang/Object;", (void *)invoke_lisp};
            if ((*env)->RegisterNatives(env, helper, &method, 1) != JNI_OK) exception(env);
            if (!error_kind) dispatch_id = (*env)->GetStaticMethodID(env, helper, "dispatch", "(ILjava/lang/Object;Ljava/lang/String;Ljava/lang/String;[Ljava/lang/Object;)Ljava/lang/Object;");
            exception(env);
            if (!error_kind) box_id = (*env)->GetStaticMethodID(env, helper, "box", "(IJDLjava/lang/String;)Ljava/lang/Object;");
            exception(env);
            if (!error_kind) kind_id = (*env)->GetStaticMethodID(env, helper, "kind", "(Ljava/lang/Object;)I");
            exception(env);
            if (!error_kind) integer_id = (*env)->GetStaticMethodID(env, helper, "integer", "(Ljava/lang/Object;)J");
            exception(env);
            if (!error_kind) real_id = (*env)->GetStaticMethodID(env, helper, "real", "(Ljava/lang/Object;)D");
            exception(env);
            if (!error_kind) take_output_id = (*env)->GetStaticMethodID(env, helper, "takeOutput", "(I)Ljava/lang/String;");
            exception(env);
            /* Redirect the standard streams before any user code can print. */
            if (!error_kind) {
                jmethodID capture = (*env)->GetStaticMethodID(env, helper, "captureStreams", "()V");
                if (!exception(env) && capture) { (*env)->CallStaticVoidMethod(env, helper, capture); exception(env); }
            }
        }
    }
    (*env)->PopLocalFrame(env, NULL);
    (*vm)->DetachCurrentThread(vm);
done:
    start->failure = error_kind;
    start->length = error_length;
    memcpy(start->message, error_text, sizeof(error_text));
    pthread_mutex_lock(&lock);
    state = error_kind ? (vm || creation_attempted ? 4 : 0) : 2;
    pthread_mutex_unlock(&lock);
    return NULL;
}
int tj_start(const char *library, const char *classpath, const char *options, int attach) {
    clear_error();
    if (!native_stack()) return 0;
    pthread_mutex_lock(&lock);
    if (state != 0) { pthread_mutex_unlock(&lock); fail("JVM already started or stopped; restart is unsupported"); return 0; }
    state = 1;
    pthread_mutex_unlock(&lock);
    Start start = {.library = library, .classpath = classpath, .options = options, .attach = attach};
    pthread_t thread;
    if (pthread_create(&thread, NULL, start_worker, &start)) {
        pthread_mutex_lock(&lock); state = 0; pthread_mutex_unlock(&lock);
        fail("Cannot create JVM initialization thread"); return 0;
    }
    pthread_join(thread, NULL);
    error_kind = start.failure; error_length = start.length;
    memcpy(error_text, start.message, sizeof(error_text));
    return !error_kind;
}
/* Everything Java wrote to stream WHICH (0=out, 1=err) since the last call, as a
 * String handle, or 0 when there was nothing -- the common case, so this stays cheap
 * enough to call at every crossing. Returning a handle rather than copying bytes out
 * lets Lisp read it through the existing tj_text path instead of new plumbing. */
Handle tj_drain_output(int which) {
    Entry entry; if (!enter(&entry)) return 0;
    JNIEnv *env = entry.env;
    Handle id = 0;
    if (take_output_id) {
        jobject text = (*env)->CallStaticObjectMethod(env, helper, take_output_id, (jint)which);
        if (!exception(env) && text) id = retain(env, text, 0);
    }
    leave(&entry); return id;
}

int tj_state(void) { pthread_mutex_lock(&lock); int result = state; pthread_mutex_unlock(&lock); return result; }

static pthread_cond_t stopped = PTHREAD_COND_INITIALIZER;
static int stop_result = JNI_ERR;
static void *stop_worker(void *unused) {
    (void)unused;
    JNIEnv *env = NULL;
    int rc = (*vm)->AttachCurrentThreadAsDaemon(vm, (void **)&env, NULL);
    if (rc == JNI_OK) {
        jclass api = (*env)->FindClass(env, "org/torcl/jvm/Api");
        if (api) {
            jmethodID clear = (*env)->GetStaticMethodID(env, api, "clear", "()V");
            if (clear) (*env)->CallStaticVoidMethod(env, api, clear);
            (*env)->DeleteLocalRef(env, api);
        }
        exception(env);
        rc = (*env)->UnregisterNatives(env, helper);
        if ((*env)->ExceptionCheck(env)) (*env)->ExceptionClear(env);
        (*env)->DeleteGlobalRef(env, helper);
        helper = NULL;
        (*env)->DeleteGlobalRef(env, ambiguity_type);
        ambiguity_type = NULL;
        if (owns_vm) rc = (*vm)->DestroyJavaVM(vm);
        else rc = (*vm)->DetachCurrentThread(vm);
    }
    pthread_mutex_lock(&lock);
    stop_result = rc;
    state = 4;
    pthread_cond_broadcast(&stopped);
    pthread_mutex_unlock(&lock);
    return NULL;
}
int tj_stop(int milliseconds) {
    clear_error();
    if (!native_stack()) return 0;
    if (milliseconds < 0) { fail("Negative shutdown timeout"); return 0; }
    pthread_mutex_lock(&lock);
    if (state == 2) {
        if (active || references || callbacks) {
            pthread_mutex_unlock(&lock); fail("Release all Java references and callbacks, and finish active calls before stopping JVM"); return 0;
        }
        state = 3;
        pthread_t thread;
        if (pthread_create(&thread, NULL, stop_worker, NULL)) {
            state = 2; pthread_mutex_unlock(&lock); fail("Cannot create shutdown thread"); return 0;
        }
        pthread_detach(thread);
    } else if (state != 3 && state != 4) {
        pthread_mutex_unlock(&lock); fail("JVM was not started"); return 0;
    }
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += milliseconds / 1000;
    deadline.tv_nsec += (milliseconds % 1000) * 1000000L;
    if (deadline.tv_nsec >= 1000000000L) { ++deadline.tv_sec; deadline.tv_nsec -= 1000000000L; }
    while (state == 3) {
        if (pthread_cond_timedwait(&stopped, &lock, &deadline)) {
            pthread_mutex_unlock(&lock); fail("JVM shutdown is still pending; non-daemon Java threads may be alive"); return 0;
        }
    }
    int result = stop_result == JNI_OK;
    pthread_mutex_unlock(&lock);
    if (!result) fail("JVM shutdown failed");
    return result;
}
