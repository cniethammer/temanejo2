/* Hybrid MPI + OpenMP test program for the temanejo task graph tool.
 *
 * Each rank runs three phases that exercise different parts of the OMPT
 * task interface:
 *   1. blocked "Cholesky-like" sweep with depend(in/out/inout) clauses
 *   2. recursive divide and conquer with taskwait (fib-style)
 *   3. nested parallel regions with taskgroup + taskloop
 *
 * Build with -DNO_MPI for an OpenMP-only variant.
 *
 * Usage: mpi_omp_tasks [blocks=24] [fib_n=17] [loop_tasks=8000]
 * The defaults give roughly 10k tasks per rank.
 */
#ifndef NO_MPI
#include <mpi.h>
#endif
#include <omp.h>
#include <stdio.h>
#include <stdlib.h>

static double burn(long iters) {
  volatile double x = 1.0;
  for (long i = 0; i < iters; i++)
    x = x * 1.0000001 + 1e-9;
  return x;
}

static void phase_dependences(int nb, int rank) {
  char *blk = calloc((size_t)nb * nb, 1); /* only used as dependence handles */
#pragma omp parallel
#pragma omp single
  {
    for (int k = 0; k < nb; k++) {
#pragma omp task depend(inout : blk[k * nb + k]) firstprivate(k)
      burn(40000 * (rank + 1)); /* "potrf" */
      for (int i = k + 1; i < nb; i++) {
#pragma omp task depend(in : blk[k * nb + k]) depend(inout : blk[i * nb + k]) firstprivate(i, k)
        burn(20000); /* "trsm" */
      }
      for (int i = k + 1; i < nb; i++) {
        for (int j = k + 1; j <= i; j++) {
#pragma omp task depend(in : blk[i * nb + k], blk[j * nb + k]) depend(inout : blk[i * nb + j]) firstprivate(i, j, k)
          burn(10000); /* "gemm/syrk" */
        }
      }
    }
  }
  free(blk);
}

static long fib(int n) {
  if (n < 2) {
    burn(2000);
    return n;
  }
  long a, b;
#pragma omp task shared(a)
  a = fib(n - 1);
#pragma omp task shared(b)
  b = fib(n - 2);
#pragma omp taskwait
  burn(500);
  return a + b;
}

static void phase_nested(int loop_tasks) {
  omp_set_max_active_levels(2);
#pragma omp parallel num_threads(2)
  {
#pragma omp parallel num_threads(2)
    {
#pragma omp single
      {
#pragma omp taskgroup
        {
#pragma omp taskloop num_tasks(loop_tasks / 4) nogroup
          for (int i = 0; i < loop_tasks; i++)
            burn(1500 + (i % 7) * 300);
        }
#pragma omp task
        burn(30000);
      }
    }
  }
}

int main(int argc, char **argv) {
  int rank = 0, size = 1;
#ifndef NO_MPI
  int provided;
  MPI_Init_thread(&argc, &argv, MPI_THREAD_FUNNELED, &provided);
  MPI_Comm_rank(MPI_COMM_WORLD, &rank);
  MPI_Comm_size(MPI_COMM_WORLD, &size);
#endif

  int nb = argc > 1 ? atoi(argv[1]) : 24;
  int fib_n = argc > 2 ? atoi(argv[2]) : 17;
  int loop_tasks = argc > 3 ? atoi(argv[3]) : 8000;

  double t0 = omp_get_wtime();
  phase_dependences(nb, rank);

  long f = 0;
#pragma omp parallel
#pragma omp single
  f = fib(fib_n + (rank % 2));

  phase_nested(loop_tasks);

  double local = omp_get_wtime() - t0, total = local;
#ifndef NO_MPI
  MPI_Reduce(&local, &total, 1, MPI_DOUBLE, MPI_SUM, 0, MPI_COMM_WORLD);
#endif
  if (rank == 0)
    printf("ranks=%d threads=%d fib=%ld sum of rank times=%.3fs\n", size, omp_get_max_threads(), f, total);

#ifndef NO_MPI
  MPI_Finalize();
#endif
  return 0;
}
