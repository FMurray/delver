# pip install delver-pdf

import delver_pdf
import cProfile
import pstats
import io


def main():
    """Main function to profile"""
    result = delver_pdf.process_pdf_file("./tests/3M_2015_10K.pdf", "./tests/10k.tmpl")
    print(result)
    return result


if __name__ == "__main__":
    # Create a cProfile object
    profiler = cProfile.Profile()

    # Run the profiler
    profiler.enable()
    main()
    profiler.disable()

    # Create a StringIO object to capture the output
    s = io.StringIO()
    ps = pstats.Stats(profiler, stream=s)

    # Sort by cumulative time and print the top functions
    ps.sort_stats("cumulative")
    ps.print_stats(20)  # Print top 20 functions

    # Print the results
    print("\n" + "=" * 50)
    print("PROFILING RESULTS")
    print("=" * 50)
    print(s.getvalue())

    # Also save to a file for detailed analysis
    ps.dump_stats("profile_results.prof")
    print("\nDetailed profiling data saved to 'profile_results.prof'")
    print("You can analyze it with: python -m pstats profile_results.prof")
