/*
 * Compile a .cu file to PTX with NVRTC, which is the only CUDA compiler this project can install
 * without root: the `nvidia-cuda-nvcc-cu12` wheel ships `ptxas` and `libnvvm` but no `nvcc`
 * frontend and no `cicc`, while `nvidia-cuda-nvrtc-cu12` ships a complete compiler as a library.
 *
 *   cc -o cu2ptx cu2ptx.c -ldl
 *   LIBNVRTC=~/cuda/nvidia/cuda_nvrtc/lib/libnvrtc.so.12 ./cu2ptx lif.cu lif.ptx compute_75
 *
 * `--fmad=false` is not optional: the bit-exactness contract needs every multiply and add to round
 * separately, exactly as the CPU kernel's f64 expressions do.
 */
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef void *nvrtcProgram;
typedef int (*create_fn)(nvrtcProgram *, const char *, const char *, int, const char **,
                         const char **);
typedef int (*compile_fn)(nvrtcProgram, int, const char **);
typedef int (*size_fn)(nvrtcProgram, size_t *);
typedef int (*get_fn)(nvrtcProgram, char *);
typedef const char *(*str_fn)(int);

int main(int argc, char **argv) {
  if (argc < 4) {
    fprintf(stderr, "usage: cu2ptx <input.cu> <output.ptx> <compute_arch>\n");
    return 2;
  }
  const char *libname = getenv("LIBNVRTC");
  if (libname == NULL) {
    libname = "libnvrtc.so.12";
  }
  void *lib = dlopen(libname, RTLD_NOW);
  if (lib == NULL) {
    fprintf(stderr, "cu2ptx: cannot load %s: %s\n", libname, dlerror());
    return 1;
  }
  create_fn create = (create_fn)dlsym(lib, "nvrtcCreateProgram");
  compile_fn compile = (compile_fn)dlsym(lib, "nvrtcCompileProgram");
  size_fn log_size = (size_fn)dlsym(lib, "nvrtcGetProgramLogSize");
  get_fn get_log = (get_fn)dlsym(lib, "nvrtcGetProgramLog");
  size_fn ptx_size = (size_fn)dlsym(lib, "nvrtcGetPTXSize");
  get_fn get_ptx = (get_fn)dlsym(lib, "nvrtcGetPTX");
  str_fn error_string = (str_fn)dlsym(lib, "nvrtcGetErrorString");
  if (!create || !compile || !log_size || !get_log || !ptx_size || !get_ptx || !error_string) {
    fprintf(stderr, "cu2ptx: %s is missing NVRTC entry points\n", libname);
    return 1;
  }

  FILE *input = fopen(argv[1], "rb");
  if (input == NULL) {
    fprintf(stderr, "cu2ptx: cannot open %s\n", argv[1]);
    return 1;
  }
  fseek(input, 0, SEEK_END);
  long length = ftell(input);
  fseek(input, 0, SEEK_SET);
  char *source = malloc((size_t)length + 1);
  if (fread(source, 1, (size_t)length, input) != (size_t)length) {
    fprintf(stderr, "cu2ptx: short read on %s\n", argv[1]);
    return 1;
  }
  source[length] = '\0';
  fclose(input);

  nvrtcProgram program = NULL;
  int status = create(&program, source, argv[1], 0, NULL, NULL);
  if (status != 0) {
    fprintf(stderr, "cu2ptx: nvrtcCreateProgram: %s\n", error_string(status));
    return 1;
  }

  char arch[64];
  snprintf(arch, sizeof(arch), "--gpu-architecture=%s", argv[3]);
  const char *options[] = {arch, "--fmad=false", "--std=c++17", "-default-device"};
  status = compile(program, 4, options);

  size_t log_bytes = 0;
  log_size(program, &log_bytes);
  if (log_bytes > 1) {
    char *log = malloc(log_bytes);
    get_log(program, log);
    fputs(log, stderr);
    free(log);
  }
  if (status != 0) {
    fprintf(stderr, "cu2ptx: nvrtcCompileProgram: %s\n", error_string(status));
    return 1;
  }

  size_t bytes = 0;
  ptx_size(program, &bytes);
  char *ptx = malloc(bytes);
  get_ptx(program, ptx);
  FILE *output = fopen(argv[2], "wb");
  if (output == NULL) {
    fprintf(stderr, "cu2ptx: cannot write %s\n", argv[2]);
    return 1;
  }
  fwrite(ptx, 1, bytes - 1, output);
  fclose(output);
  fprintf(stderr, "cu2ptx: wrote %s (%zu bytes) for %s\n", argv[2], bytes - 1, argv[3]);
  return 0;
}
