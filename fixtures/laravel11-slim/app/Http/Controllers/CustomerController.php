<?php

namespace App\Http\Controllers;

use App\Models\Customer;
use Illuminate\Http\Request;

class CustomerController extends Controller
{
    public function index()
    {
        return Customer::paginate();
    }

    public function store(Request $request)
    {
        return Customer::create($request->only('name', 'email'));
    }

    public function show(Customer $customer)
    {
        return $customer;
    }

    public function update(Request $request, Customer $customer)
    {
        $customer->update($request->only('name', 'email'));

        return $customer;
    }
}
