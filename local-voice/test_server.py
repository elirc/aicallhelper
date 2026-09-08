import unittest
from server import TranscriptState, pcm_floats
from install_ollama import cpu_file


class ProtocolTests(unittest.TestCase):
    def test_interim_revisions_replace_text_and_preserve_lines(self):
        state = TranscriptState()
        self.assertEqual(state.update(9, "What is"), "What is")
        self.assertEqual(state.update(9, "What is Rust?"), "What is Rust?")
        self.assertEqual(state.update(12, " Explain ownership."), "What is Rust? Explain ownership.")
        self.assertEqual(state.update(9, "What is Rust exactly?"), "What is Rust exactly? Explain ownership.")

    def test_pcm_is_little_endian_normalized_and_validated(self):
        self.assertEqual(pcm_floats(bytes([0, 128, 0, 0, 255, 127])), [-1, 0, 32767 / 32768])
        with self.assertRaises(ValueError):
            pcm_floats(b"x")

    def test_cpu_package_keeps_cpu_and_licenses(self):
        for name in ["ollama.exe", "lib/ollama/ggml-cpu-haswell.dll", "lib/ollama/ggml-base.dll", "LICENSE"]:
            self.assertTrue(cpu_file(name))
        for name in ["lib/ollama/cuda_v12/cublas64_12.dll", "lib/ollama/vulkan/ggml-vulkan.dll", "lib/ollama/rocm/rocblas.dll"]:
            self.assertFalse(cpu_file(name))


if __name__ == "__main__":
    unittest.main()
