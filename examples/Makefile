# Builds the example with clang's OpenMP runtime (libomp), which provides OMPT.
# Open MPI's wrapper is told to use clang through OMPI_CC.
CC      = clang
MPICC   = mpicc
# -fno-optimize-sibling-calls keeps the call site of constructs that end a
# function or region body, so the tool can report their source line
# (see docs/runtime-notes.rst).
CFLAGS  = -fopenmp -g -O1 -fno-optimize-sibling-calls

all: mpi_omp_tasks omp_tasks

mpi_omp_tasks: mpi_omp_tasks.c
	OMPI_CC=$(CC) $(MPICC) $(CFLAGS) $< -o $@

# OpenMP-only variant, no MPI needed
omp_tasks: mpi_omp_tasks.c
	$(CC) -DNO_MPI $(CFLAGS) $< -o $@

clean:
	rm -f mpi_omp_tasks omp_tasks

.PHONY: all clean
