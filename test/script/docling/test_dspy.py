#!/usr/bin/env python3
import dspy
import litellm

base_url = "https://model.inferx.net/funccall/tn-a3t79iogb2/endpoints/Qwen3.6-35B-A3B-FP8/v1"
api_key = "ix_0929e8b02da13c0437ec37b4a41ba46e936d59c92ae5605a3c3448953d621ba0"
model_name = "Qwen/Qwen3.6-35B-A3B-FP8"

print("Testing LiteLLM directly with Qwen3.6-35B-A3B-FP8...")
print(f"Base URL: {base_url}")

# Test with completion directly using the exact model name
print("\nTest 1: Using litellm.completion with model=Qwen/Qwen3.6-35B-A3B-FP8")
try:
    result = litellm.completion(
        model=model_name,
        messages=[{"role": "user", "content": "Say hello"}],
        api_base=base_url,
        api_key=api_key,
        custom_llm_provider="openai"
    )
    print(f"Success! Result: {result.choices[0].message.content}")
except Exception as e:
    print(f"Failed: {e}")

# Test 2: With DSPy Predict
print("\nTest 2: Using DSPy with Predict")
try:
    lm = dspy.LM(f"huggingface/{model_name}", api_base=base_url, api_key=api_key)
    dspy.configure(lm=lm)
    
    class TestModule(dspy.Module):
        def __init__(self):
            super().__init__()
            self.answer = dspy.Predict("question -> response")
        def forward(self, question):
            return self.answer(question=question)
    
    test = TestModule()
    response = test("What is 2+2?")
    print(f"Success! Response: {response.response}")
except Exception as e:
    print(f"Failed: {e}")

# Test 3: With DSPy Signature
print("\nTest 3: Using DSPy Signature with Predict")
try:
    class TestSignature(dspy.Signature):
        raw_markdown = dspy.InputField(desc="Raw Markdown content")
        optimized_markdown = dspy.OutputField(desc="Optimized Markdown")
    
    optimizer = dspy.Predict(TestSignature)
    test_content = "# Header\n\nSome text"
    result = optimizer(raw_markdown=test_content)
    print(f"Result type: {type(result)}")
    print(f"Result: {result}")
    print(f"optimized_markdown: {getattr(result, 'optimized_markdown', 'N/A')}")
except Exception as e:
    import traceback
    print(f"Failed: {e}")
    traceback.print_exc()

# Test 2: With DSPy Predict
print("\nTest 2: Using DSPy with Predict")
try:
    lm = dspy.LM(model_name, api_base=base_url, api_key=api_key)
    dspy.configure(lm=lm)
    
    class TestModule(dspy.Module):
        def __init__(self):
            super().__init__()
            self.answer = dspy.Predict("question -> response")
        def forward(self, question):
            return self.answer(question=question)
    
    test = TestModule()
    response = test("What is 2+2?")
    print(f"Success! Response: {response.response}")
except Exception as e:
    print(f"Failed: {e}")
