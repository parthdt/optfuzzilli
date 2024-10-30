import fs
import Foundation

func testFzilScheduler() {
    // Create a new scheduler
    let scheduler = MyFzilScheduler() // Direct initialization
    print("Scheduler created successfully")

    // Test adding an input using Data
    let testInput = "Hello, Fuzzing!".data(using: .utf8)!
    scheduler.addInput(inputData: testInput)
    print("Added input: \(testInput)")

    // Fetch and print the current test case as Data
    let currentTestcaseData = scheduler.nextInput()
    print("Current Testcase: \(String(data: currentTestcaseData, encoding: .utf8) ?? "Invalid Data")")

    // Add another input and get the next input from the scheduler
    let anotherInput = "Another test case".data(using: .utf8)!
    scheduler.addInput(inputData: anotherInput)
    print("Added input: \(anotherInput)")

    // Fetch and print the next input from the scheduler as Data
    let nextInputData = scheduler.nextInput()
    print("Next Input: \(String(data: nextInputData, encoding: .utf8) ?? "Invalid Data")")
}

// Call the test function
testFzilScheduler()

